#!/bin/bash
# Run README.md's install guide verbatim in a fresh container.
#
#   tests/readme_install/run.sh ubuntu|fedora|arch [--user] [README]
#
# Default: a root shell, as the images come. --user (ubuntu): an ordinary user
# with sudo, like a desktop. Each run: install (steps 1-6) -> checks ->
# update -> checks -> uninstall -> checks that nothing is left.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
distro=$1; mode=${2:-root}; readme=${3:-$here/../../README.md}
case $distro in ubuntu) img=ubuntu:24.04;; fedora) img=fedora:latest;; arch) img=archlinux:latest;; esac
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
chmod 755 "$work"  # readable by the --user account
flags=(--unattended); [ "$mode" = --user ] || flags+=(--root)
for phase in install update uninstall; do
  python3 "$here/extract.py" "$readme" "$distro" "$phase" "${flags[@]}" > "$work/$phase.sh"
done
cp "$here/checks.sh" "$work/"
# Scaffolding only, not part of the guide: python3 for the CLI round-trip,
# and in --user mode a sudo-capable user (a desktop already has one).
case $distro in
  ubuntu) prep='apt-get update -qq && apt-get install -y -qq python3 >/dev/null';;
  fedora) prep='dnf install -y -q python3 >/dev/null';;
  # The arch image's root has no ~/.bashrc, so nvm finds no profile to add
  # itself to; a real account is made from /etc/skel and has one.
  arch)   prep='pacman -Sy --noconfirm --needed python >/dev/null && cp -n /etc/skel/.bash* ~/';;
esac
# Update and Uninstall happen later, in a new terminal: `bash -i` loads the
# ~/.bashrc lines rustup and nvm added, exactly as a new terminal would.
steps='set -e; cd ~; bash /w/install.sh; bash /w/checks.sh installed '$distro'
       cd ~/bellman; bash -i /w/update.sh; bash -i /w/checks.sh installed '$distro'
       cd ~/bellman; bash -i /w/uninstall.sh; bash /w/checks.sh removed '$distro
if [ "$mode" = --user ]; then
  prep="$prep && apt-get install -y -qq sudo >/dev/null && useradd -m -s /bin/bash builder \
        && echo 'builder ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/builder"
  inner="su - builder -c \"\$(cat /w/steps)\""
else
  inner='bash /w/steps'
fi
printf '%s\n' "$steps" > "$work/steps"
docker run --rm --cpus "${CPUS:-8}" -v "$work:/w:ro" "$img" bash -c "$prep && $inner"
