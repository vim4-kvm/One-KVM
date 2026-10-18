use super::Target;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const CLEAR: &str = "\x1b[2J\x1b[H";
#[derive(Clone, PartialEq, Eq)]
pub struct Item {
    pub target: Target,
    pub detail: String,
    pub unavailable: Option<String>,
    pub busy: Option<(u32, String)>,
}
impl Item {
    pub fn label(&self) -> &str {
        match self.target {
            Target::Board => "KVM 板子",
            Target::Serial => "被控机串口",
            Target::Ssh => "被控机 SSH",
            Target::Picker => "选择终端",
        }
    }
    fn detail(&self) -> String {
        if let Some(reason) = &self.unavailable {
            reason.clone()
        } else if let Some((_, name)) = &self.busy {
            format!("正在「{name}」中使用")
        } else {
            self.detail.clone()
        }
    }
}
pub fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        result.push(c);
        used += w;
    }
    result.push('…');
    result
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    Quit,
    Open(Target, Option<u32>),
}
#[derive(Default)]
pub struct Picker {
    pub selected: Option<usize>,
    pub confirm: Option<u32>,
    pub footer: Option<(String, &'static str)>,
    escape: u8,
    carriage_return: bool,
}
impl Picker {
    pub fn new(items: &[Item], last: Option<Target>) -> Self {
        Self {
            selected: items
                .iter()
                .position(|i| i.unavailable.is_none() && Some(i.target) == last)
                .or_else(|| items.iter().position(|i| i.unavailable.is_none())),
            ..Default::default()
        }
    }
    pub fn refresh(&mut self, items: &[Item]) {
        if self
            .selected
            .is_none_or(|i| items.get(i).is_none_or(|i| i.unavailable.is_some()))
        {
            self.selected = items.iter().position(|i| i.unavailable.is_none());
        }
        if self.confirm.take().is_some() {
            self.footer = None;
        }
    }
    pub fn key(&mut self, byte: u8, items: &[Item]) -> Action {
        // CRLF represents one Enter, including when split across channel frames.
        // Its LF must never satisfy the second confirmation for serial takeover.
        let paired_lf = self.carriage_return && byte == b'\n';
        self.carriage_return = byte == b'\r';
        if paired_lf {
            return Action::None;
        }
        // Escape sequences may arrive split across data-channel messages.
        if self.escape != 0 {
            let previous = self.escape;
            self.escape = 0;
            if previous == 1 && matches!(byte, b'[' | b'O') {
                self.escape = 2;
                return Action::None;
            }
            if previous == 2 && matches!(byte, b'A' | b'B') {
                return self.move_by(items, byte == b'B');
            }
            return Action::None;
        }
        if let Some(id) = self.confirm.take() {
            self.footer = None;
            if matches!(byte, b'\r' | b'\n')
                && self
                    .selected
                    .and_then(|i| items.get(i))
                    .is_some_and(|i| i.busy.as_ref().is_some_and(|b| b.0 == id))
            {
                return Action::Open(Target::Serial, Some(id));
            }
            return Action::Redraw;
        }
        match byte {
            27 => {
                self.escape = 1;
                Action::None
            }
            b'q' | 3 | 4 => Action::Quit,
            b'k' => self.move_by(items, false),
            b'j' => self.move_by(items, true),
            b'1'..=b'3' => self.open((byte - b'1') as usize, items),
            b'\r' | b'\n' => self.selected.map_or(Action::None, |i| self.open(i, items)),
            _ => Action::None,
        }
    }
    fn move_by(&mut self, items: &[Item], down: bool) -> Action {
        let count = items.len();
        if count == 0 {
            return Action::None;
        }
        let start = self.selected.unwrap_or(if down { count - 1 } else { 0 });
        self.selected = (1..=count)
            .map(|step| {
                if down {
                    (start + step) % count
                } else {
                    (start + count - step) % count
                }
            })
            .find(|&i| items[i].unavailable.is_none());
        self.footer = None;
        Action::Redraw
    }
    fn open(&mut self, i: usize, items: &[Item]) -> Action {
        let Some(item) = items.get(i) else {
            return Action::None;
        };
        if let Some(reason) = &item.unavailable {
            self.footer = Some((reason.clone(), "33"));
            return Action::Redraw;
        }
        self.selected = Some(i);
        if let Some((id, name)) = &item.busy {
            self.confirm = Some(*id);
            self.footer = Some((
                format!("再按回车接管：会话「{name}」将结束 · 其他键取消"),
                "33",
            ));
            Action::Redraw
        } else {
            Action::Open(item.target, None)
        }
    }
    pub fn render(&self, name: &str, cols: u16, items: &[Item]) -> String {
        let width = usize::from(cols);
        let narrow = cols < 52;
        let mut out = format!(
            "{CLEAR}\x1b[1m{}\x1b[0m\r\n\r\n",
            truncate(&format!(" {name} 终端"), width)
        );
        let label_width = items
            .iter()
            .map(|i| UnicodeWidthStr::width(i.label()))
            .max()
            .unwrap_or(0);
        for (n, item) in items.iter().enumerate() {
            let selected = self.selected == Some(n);
            let color = if item.unavailable.is_some() {
                "2"
            } else if selected {
                "1;36"
            } else {
                "0"
            };
            let prefix = format!(
                " {} {}  {}",
                if selected { "›" } else { " " },
                n + 1,
                item.label()
            );
            let line = if narrow {
                prefix
            } else {
                format!(
                    "{prefix}{}{}",
                    " ".repeat(label_width - UnicodeWidthStr::width(item.label()) + 3),
                    item.detail()
                )
            };
            out.push_str(&format!(
                "\x1b[{color}m{}\x1b[0m\r\n",
                truncate(&line, width)
            ));
            if narrow {
                out.push_str(&format!(
                    "\x1b[2m{}\x1b[0m\r\n",
                    truncate(&format!("      {}", item.detail()), width)
                ));
            }
        }
        let (footer, color) = self
            .footer
            .as_ref()
            .map(|(s, c)| (s.as_str(), *c))
            .unwrap_or(("↑↓ 选择 · 回车打开 · 数字直接打开 · q 退出", "2"));
        out.push_str(&format!(
            "\r\n\x1b[{color}m{}\x1b[0m",
            truncate(footer, width)
        ));
        out
    }
}
pub fn direct_target(items: &[Item]) -> Option<Target> {
    if items.len() == 1 && items[0].unavailable.is_none() && items[0].busy.is_none() {
        Some(items[0].target)
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn items() -> Vec<Item> {
        vec![
            Item {
                target: Target::Board,
                detail: "khadas@vim4-kvm".into(),
                unavailable: None,
                busy: None,
            },
            Item {
                target: Target::Serial,
                detail: "USB 串口 · 115200".into(),
                unavailable: Some("未检测到设备".into()),
                busy: None,
            },
            Item {
                target: Target::Ssh,
                detail: "user@host".into(),
                unavailable: None,
                busy: None,
            },
        ]
    }
    #[test]
    fn skips_unavailable_and_supports_split_arrows_and_numbers() {
        let items = items();
        let mut p = Picker::new(&items, None);
        for b in b"\x1b[B" {
            p.key(*b, &items);
        }
        assert_eq!(p.selected, Some(2));
        for b in b"\x1bOA" {
            p.key(*b, &items);
        }
        assert_eq!(p.selected, Some(0));
        assert_eq!(p.key(b'2', &items), Action::Redraw);
        assert!(p.footer.is_some());
        assert_eq!(p.key(b'3', &items), Action::Open(Target::Ssh, None));
        assert_eq!(direct_target(&items[..1]), Some(Target::Board));
        assert_eq!(direct_target(&items[1..2]), None);
    }
    #[test]
    fn takeover_requires_enter_and_other_keys_cancel() {
        let mut items = items();
        items[1].unavailable = None;
        items[1].busy = Some((7, "串口".into()));
        let mut p = Picker::new(&items, None);
        assert_eq!(p.key(b'2', &items), Action::Redraw);
        assert_eq!(p.key(b'2', &items), Action::Redraw);
        assert_eq!(p.confirm, None);
        p.key(b'2', &items);
        assert_eq!(p.key(b'\r', &items), Action::Open(Target::Serial, Some(7)));
        assert_eq!(direct_target(&items[1..2]), None);
        let mut p = Picker::new(&items, Some(Target::Serial));
        assert_eq!(p.key(b'\r', &items), Action::Redraw);
        assert_eq!(p.key(b'\n', &items), Action::None);
        assert_eq!(p.key(b'\r', &items), Action::Open(Target::Serial, Some(7)));
    }
    #[test]
    fn wide_and_narrow_render_use_display_width() {
        let items = items();
        let p = Picker::new(&items, None);
        let wide = p.render("vim4-kvm", 80, &items);
        assert!(wide.contains("KVM 板子     khadas@vim4-kvm"));
        let narrow = p.render("vim4-kvm", 30, &items);
        assert!(narrow.contains("      khadas@vim4-kvm"));
        assert_eq!(truncate("板子abcdef", 6), "板子a…");
        assert_eq!(truncate("板", 1), "…");
        assert!(wide.contains("\x1b[2m   2"));
    }
}
