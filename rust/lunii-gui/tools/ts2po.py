#!/usr/bin/env python3
"""Converts the Qt translation file (locales/fr_FR.ts) to gettext .po for the Slint GUI.

Usage: python3 ts2po.py ../../locales/fr_FR.ts extra_fr.po > ../lang/fr/LC_MESSAGES/lunii-gui.po

Contexts are dropped (the Slint GUI uses a single translation domain). When the
same source text is translated differently in several contexts, the first one wins.
`extra_fr.po` holds the translations of strings that only exist in the Rust version.
"""
import sys
import xml.etree.ElementTree as ET


def po_escape(s):
    return s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\t", "\\t")


def main():
    ts_file = sys.argv[1]
    extra = sys.argv[2] if len(sys.argv) > 2 else None

    entries = {}
    for context in ET.parse(ts_file).getroot().iter("context"):
        for msg in context.iter("message"):
            source = msg.findtext("source") or ""
            tr = msg.find("translation")
            if tr is None or tr.get("type") in ("vanished", "obsolete", "unfinished") or not (tr.text or "").strip():
                continue
            entries.setdefault(source, tr.text)
            # Qt mnemonics ("&File") are not used by Slint
            if "&" in source and "&" in tr.text:
                entries.setdefault(source.replace("&", ""), tr.text.replace("&", ""))

    out = sys.stdout
    out.write('msgid ""\nmsgstr ""\n"Content-Type: text/plain; charset=UTF-8\\n"\n"Language: fr\\n"\n\n')
    out.write("# Converted from locales/fr_FR.ts by tools/ts2po.py\n\n")
    for source, translation in entries.items():
        out.write(f'msgid "{po_escape(source)}"\nmsgstr "{po_escape(translation)}"\n\n')
    if extra:
        out.write("# Strings specific to the Rust version (tools/extra_fr.po)\n\n")
        with open(extra, encoding="utf-8") as f:
            out.write(f.read())


if __name__ == "__main__":
    main()
