# Email test certificates

Throwaway test material for the in-test SMTP server, never used outside tests:

- `ca.der`: a self-signed P-256 CA ("Lodestar test CA"), trusted only by the email tests.
- `server.der`: a leaf for `DNS:mail.test`, signed by that CA, valid until 2126.
- `server.key.der`: the leaf's PKCS#8 private key.

Made with `openssl req -x509` (CA) and `openssl x509 -req` (leaf), then converted to DER, so the tests need no PEM parser.
