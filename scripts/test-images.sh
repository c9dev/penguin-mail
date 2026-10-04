#!/bin/sh
# Pulls the mail and calendar servers the Docker-backed tests start, each
# pinned by digest. The tests never pull: without an image they skip, or
# fail under PENGUIN_MAIL_REQUIRE_IMAP. Run this once on a new computer
# and after a digest changes in testmail/src/lib.rs.
set -eu
for image in \
    "dovecot/dovecot:2.4.5@sha256:c807be4fb5a97d9c3a90770569d3a6c4cbdcb36742ad41f90409cbd929166553" \
    "axllent/mailpit:v1.31.2@sha256:74d609a42ec279aa63c6b4622a6fa9b5408d1ad5b1d76a1c4be40a265ce0863d" \
    "ghcr.io/kozea/radicale:3.8.1@sha256:54d9406cd30f9e206a9dd6d61dfac48da26b9afc37200e14f23beb8fafa7d11c"
do
    docker pull --quiet "$image"
done
