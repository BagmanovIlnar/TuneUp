# nfpm config — version is injected by scripts/ci/package-linux.sh
name: tuneup
arch: amd64
platform: linux
version: ${VERSION}
release: "1"
section: utils
priority: optional
maintainer: TuneUp Maintainers <tuneup@example.invalid>
description: |-
  TuneUp monitors background applications and can put them to sleep
  (terminate + disable autostart), clean caches, and uninstall leftovers.
vendor: TuneUp
homepage: https://github.com/tuneup/tuneup
license: MIT
contents:
  - src: target/release/tuneup
    dst: /usr/bin/tuneup
    file_info:
      mode: 0755
  - src: target/release/tuneup-helper
    dst: /usr/libexec/tuneup-helper
    file_info:
      mode: 0755
  - src: crates/tuneup-linux/assets/com.tuneup.platform.policy
    dst: /usr/share/polkit-1/actions/com.tuneup.platform.policy
    file_info:
      mode: 0644
  - src: packaging/linux/tuneup.desktop
    dst: /usr/share/applications/tuneup.desktop
    file_info:
      mode: 0644
scripts:
  postinstall: packaging/linux/postinstall.sh
overrides:
  deb:
    depends:
      - libgtk-3-0
      - libayatana-appindicator3-1
      - policykit-1
      - libc6
  rpm:
    depends:
      - gtk3
      - libappindicator-gtk3
      - polkit
      - glibc
