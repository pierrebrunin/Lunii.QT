#!/usr/bin/env python3
"""Draws the numbered red frames of res/screenshot_interface.png (legend in the
README) on the `05-sizes` screenshot of the fake Lunii (tools/fake-lunii.sh).

Usage: annotate_interface.py <fr-05-sizes.png> <output.png>
Needs Pillow. The frames assume a 1000x650 window and the stories of
lunii-core/examples/fake_lunii.rs: 6 visible rows, then 2 hidden ones.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

RED = (214, 40, 40)
FONT = Path(__file__).resolve().parent.parent / "res/fonts/DejaVuSans-Bold.ttf"


def row_top(row: int) -> int:
    """Top of a table row (row_center() in lunii-gui/src/screenshot.rs, minus 13)."""
    return 111 + 26 * row


VISIBLE, HIDDEN = 6, 2
hidden_top, hidden_bottom = row_top(VISIBLE) - 1, row_top(VISIBLE + HIDDEN) - 1

# (frame, number position), in the order of the README legend
FRAMES = [
    ((4, 5, 311, 32), (318, 8)),  # 1 menu bar
    ((104, 43, 268, 81), (274, 51)),  # 2 Stories / Podcasts views
    ((5, 45, 62, 78), (68, 51)),  # 3 night mode
    ((962, 44, 997, 80), (944, 51)),  # 4 official database refresh
    ((5, 84, 656, 619), (631, 594)),  # 5 story list
    ((444, 88, 490, hidden_bottom), (495, 92)),  # 6 night mode column
    ((7, hidden_top, 650, hidden_bottom), (631, hidden_bottom + 9)),  # 7 hidden stories
    ((377, 621, 968, 647), (355, 625)),  # 8 status bar
]


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    image = Image.open(sys.argv[1]).convert("RGB")
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(str(FONT), 20)
    for number, (frame, position) in enumerate(FRAMES, 1):
        draw.rectangle(frame, outline=RED, width=2)
        draw.text(position, str(number), fill=RED, font=font)
    image.save(sys.argv[2])


if __name__ == "__main__":
    main()
