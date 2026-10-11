"""Small ANSI screen reader for the escape sequences emitted by Ratatui."""
import codecs
import re

class Screen:
    def __init__(self, width, height):
        self.width, self.height = width, height
        self.cells = [[(" ", "#dce6ee", "#0f161e") for _ in range(width)] for _ in range(height)]
        self.x = self.y = 0
        self.fg, self.bg = "#dce6ee", "#0f161e"
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def feed(self, data):
        text = self.pending + self.decoder.decode(data)
        self.pending = ""
        i = 0
        while i < len(text):
            char = text[i]
            if char == "\x1b":
                match = re.match(r"\x1b\[([0-9;?]*)([@-~])", text[i:])
                if match is None:
                    self.pending = text[i:]
                    break
                raw, command = match.groups()
                values = [int(v or "0") for v in raw.lstrip("?").split(";")]
                n = values[0] or 1
                if command in "Hf":
                    self.y = (values[0] or 1) - 1
                    self.x = (values[1] if len(values) > 1 else 1) - 1
                elif command == "G": self.x = n - 1
                elif command == "d": self.y = n - 1
                elif command == "A": self.y -= n
                elif command == "B": self.y += n
                elif command == "C": self.x += n
                elif command == "D": self.x -= n
                elif command == "J" and values[0] in (2, 3):
                    self.cells = [[(" ", self.fg, self.bg) for _ in range(self.width)] for _ in range(self.height)]
                elif command == "m":
                    j = 0
                    while j < len(values):
                        v = values[j]
                        if v == 0: self.fg, self.bg = "#dce6ee", "#0f161e"
                        elif v == 39: self.fg = "#dce6ee"
                        elif v == 49: self.bg = "#0f161e"
                        elif v in (38, 48) and j + 4 < len(values) and values[j+1] == 2:
                            color = "#%02x%02x%02x" % tuple(values[j+2:j+5])
                            if v == 38: self.fg = color
                            else: self.bg = color
                            j += 4
                        j += 1
                i += len(match.group())
                continue
            if char == "\n": self.y += 1; self.x = 0
            elif char == "\r": self.x = 0
            elif char >= " ":
                if 0 <= self.y < self.height and 0 <= self.x < self.width:
                    self.cells[self.y][self.x] = (char, self.fg, self.bg)
                self.x += 1
            i += 1

    @property
    def text(self):
        return "\n".join("".join(c[0] for c in row) for row in self.cells)

    def html(self):
        import html
        lines = []
        for row in self.cells:
            parts = []
            for char, fg, bg in row:
                parts.append(f'<span style="color:{fg};background:{bg}">{html.escape(char)}</span>')
            lines.append("".join(parts))
        return '<!doctype html><meta charset="utf-8"><title>dev-sweep terminal capture</title><style>body{background:#0f161e;margin:28px}pre{font:15px/1.45 "DejaVu Sans Mono",monospace;white-space:pre;margin:0}</style><pre>' + "\n".join(lines) + "</pre>"
