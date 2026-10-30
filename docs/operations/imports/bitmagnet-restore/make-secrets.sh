#!/bin/sh
# Writes two random passwords into /secrets (the admin's, which the image
# needs once, and the importer's), readable only by the container's
# postgres user (uid 70 in postgres:16-alpine). Never overwrites.
set -eu
umask 077
for name in admin_pw reader_pw; do
  if [ -e "/secrets/$name" ]; then
    echo "kept existing /secrets/$name"
  else
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "/secrets/$name"
    echo "wrote /secrets/$name"
  fi
done
chown 70:70 /secrets/admin_pw /secrets/reader_pw
chmod 600 /secrets/admin_pw /secrets/reader_pw
