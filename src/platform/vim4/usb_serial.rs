//! Read-only ACM discovery: do not create configfs instances or load modules
//! while validating a settings request (active HID handles may still be open).
use std::path::Path;

pub(super) fn available() -> bool {
    available_at(Path::new("/"))
}

fn available_at(root: &Path) -> bool {
    if root
        .join("sys/kernel/config/usb_gadget/kvm/functions/acm.usb0")
        .is_dir()
        || root.join("sys/module/usb_f_acm").is_dir()
    {
        return true;
    }
    let Ok(release) = std::fs::read_to_string(root.join("proc/sys/kernel/osrelease")) else {
        return false;
    };
    let modules = root.join("lib/modules").join(release.trim());
    ["", ".gz", ".xz", ".zst"].iter().any(|suffix| {
        modules
            .join(format!(
                "kernel/drivers/usb/gadget/function/usb_f_acm.ko{suffix}"
            ))
            .is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> std::path::PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("onekvm-acm-{}-{id}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn detects_only_running_kernel_modules_without_mutating_gadget() {
        let directory = test_root();
        let root = directory.as_path();
        std::fs::create_dir_all(root.join("proc/sys/kernel")).unwrap();
        std::fs::write(root.join("proc/sys/kernel/osrelease"), "7.3-test\n").unwrap();
        let function = root.join("lib/modules/7.3-test/kernel/drivers/usb/gadget/function");
        std::fs::create_dir_all(&function).unwrap();
        assert!(!available_at(root));
        let other = root.join("lib/modules/5.15/kernel/drivers/usb/gadget/function");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("usb_f_acm.ko"), []).unwrap();
        assert!(!available_at(root));
        for suffix in ["", ".gz", ".xz", ".zst"] {
            let module = function.join(format!("usb_f_acm.ko{suffix}"));
            std::fs::write(&module, []).unwrap();
            assert!(available_at(root));
            std::fs::remove_file(module).unwrap();
        }
        assert!(!root.join("sys").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn accepts_loaded_or_existing_function_without_module_files() {
        for path in [
            "sys/module/usb_f_acm",
            "sys/kernel/config/usb_gadget/kvm/functions/acm.usb0",
        ] {
            let root = test_root();
            std::fs::create_dir_all(root.join(path)).unwrap();
            assert!(available_at(&root));
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
