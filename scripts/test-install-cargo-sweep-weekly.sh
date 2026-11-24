#!/bin/bash
# Test for install-cargo-sweep-weekly.sh with launchctl stubbed on PATH and HOME in a temp folder.
# The real launchd is never touched. Asserts the old job is booted out and disabled, nothing else is.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"; T="$(cd -P "$T" && pwd)"
mkdir -p "$T/bin" "$T/home/Library/LaunchAgents"
LA="$T/home/Library/LaunchAgents"
plist() { printf '<plist><dict><key>ProgramArguments</key><array><string>%s</string></array></dict></plist>\n' "$2" > "$LA/$1.plist"; }
plist dev.old.cargo-sweep /usr/local/bin/cargo-sweep
plist dev.other.cargo-sweep /usr/bin/backup-tool
cat > "$T/bin/launchctl" <<F
#!/bin/bash
echo "\$*" >> "$T/calls"
[ "\$1" = list ] && printf -- '- 0 dev.old.cargo-sweep\\n- 0 dev.other.cargo-sweep\\n- 0 com.unrelated.job\\n- 0 dev.camerata.cargo-sweep-weekly\\n'
exit 0
F
chmod +x "$T/bin/launchctl"; : > "$T/calls"
out="$(env HOME="$T/home" PATH="$T/bin:$PATH" bash "$here/install-cargo-sweep-weekly.sh" 2>&1)"
fail=0
ok() { if [ "$2" = "$3" ]; then echo "PASS $1 ($3)"; else echo "FAIL $1 expected=$2 got=$3"; fail=1; fi; }
has() { grep -qF "$1" "$T/calls" && echo yes || echo no; }
u="$(id -u)"
ok "old label disabled" yes "$(has "disable gui/$u/dev.old.cargo-sweep")"
ok "old label booted out" yes "$(has "bootout gui/$u/dev.old.cargo-sweep")"
ok "other .cargo-sweep job (not running cargo-sweep) untouched" no "$(grep -E 'bootout|disable' "$T/calls" | grep -qF dev.other && echo yes || echo no)"
ok "unrelated job untouched" no "$(grep -E 'bootout|disable' "$T/calls" | grep -qF com.unrelated && echo yes || echo no)"
ok "only one disable call" 1 "$(grep -c '^disable' "$T/calls")"
echo "$out" | grep -qF "launchctl enable gui/$u/dev.old.cargo-sweep" && r=yes || r=no; ok "prints how to undo" yes "$r"
[ -f "$LA/dev.old.cargo-sweep.plist" ] && r=kept || r=gone; ok "old plist left in place" kept "$r"
[ "$fail" = 0 ] && echo "ALL PASS" || echo "FAILED"; exit "$fail"
