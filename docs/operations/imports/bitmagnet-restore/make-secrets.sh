#!/bin/sh
# Writes two random passwords into /secrets (the admin's, which the image
# needs once, and the importer's), readable only by the container's
# postgres user (uid 70 in postgres:16-alpine). Never overwrites.
set -eu
set -o pipefail
umask 077
for name in admin_pw reader_pw; do
  if [ -e "/secrets/$name" ]; then
    echo "kept existing /secrets/$name"
  else
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "/secrets/$name"
    # 32 random bytes as 64 hex digits; anything else means a step failed.
    if [ "$(wc -c < "/secrets/$name")" -ne 64 ]; then
      rm -f "/secrets/$name"
      echo "STOP: couldn't write /secrets/$name" >&2
      exit 1
    fi
    echo "wrote /secrets/$name"
  fi
done
chown 70:70 /secrets/admin_pw /secrets/reader_pw
chmod 600 /secrets/admin_pw /secrets/reader_pw
