#!/usr/bin/env python3
"""Regenerate the UU "no HDMI signal" placeholder access units.

Needs Ubuntu ffmpeg (libx264, libx265), python3-pil and fonts-noto-cjk.
Each output is one SDR BT.709 IDR with in-band parameter sets, so a viewer can
decode it alone and switch to the real stream on its next IDR. ffmpeg 6.1
drawtext truncates multibyte text, so Pillow renders the card.
"""
import pathlib
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

OUT = pathlib.Path(__file__).resolve().parent
SIZE = (1280, 720)
LINES = [  # text, style, size, top, gray level
    ('未检测到 HDMI 信号', 'Bold', 60, 250, 255),
    ('No HDMI signal', 'Bold', 40, 335, 255),
    ('被控机可能已休眠，可尝试按键或移动鼠标唤醒，或检查 HDMI 线缆', 'Regular', 26, 440, 176),
    ('The computer may be asleep. Press a key or move the mouse to wake it, '
     'or check the HDMI cable.', 'Regular', 22, 484, 176),
]


def font(style, size):
    match = subprocess.run(['fc-match', '--format=%{file}\n%{index}', f'Noto Sans CJK SC:style={style}'],
                           check=True, capture_output=True, text=True).stdout.split('\n')
    if 'NotoSansCJK' not in match[0]:
        raise SystemExit(f'Noto Sans CJK SC {style} not installed (fonts-noto-cjk)')
    return ImageFont.truetype(match[0], size, index=int(match[1]))


def main():
    card = Image.new('RGB', SIZE, (32, 32, 32))
    draw = ImageDraw.Draw(card)
    for text, style, size, top, gray in LINES:
        face = font(style, size)
        width = draw.textlength(text, font=face)
        draw.text(((SIZE[0] - width) / 2, top), text, font=face, fill=(gray, gray, gray))
    with tempfile.TemporaryDirectory() as work:
        image = pathlib.Path(work) / 'card.png'
        card.save(image)
        for encoder, name, args in [
            ('libx264', 'no_signal.h264', ['-profile:v', 'baseline', '-x264-params', 'repeat-headers=1', '-f', 'h264']),
            # keyint=1 would make x265 signal Main Intra (RExt), which Main receivers reject.
            ('libx265', 'no_signal.h265', ['-profile:v', 'main', '-x265-params',
                                           'repeat-headers=1:info=0:log-level=error', '-f', 'hevc']),
        ]:
            subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-y', '-i', str(image),
                            '-frames:v', '1', '-vf', 'scale=out_color_matrix=bt709:out_range=tv,format=yuv420p',
                            '-color_range', 'tv', '-colorspace', 'bt709', '-color_primaries', 'bt709',
                            '-color_trc', 'bt709', '-c:v', encoder, '-crf', '18', *args, str(OUT / name)],
                           check=True)


if __name__ == '__main__':
    main()
