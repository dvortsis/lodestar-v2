# minisign interop vectors

Made with the real minisign 0.11 tool (Ubuntu's `minisign` package), with a throwaway key that has no password and was deleted afterwards. TEST ONLY: nothing trusts this key.

```sh
minisign -G -W -p tool.pub -s tool.key
minisign -S -s tool.key -m message.txt -x prehashed.minisig -t 'lodestar test vector'
minisign -S -l -s tool.key -m message.txt -x legacy.minisig -t 'lodestar test vector'
```

`prehashed.minisig` must verify; `legacy.minisig` must be refused (legacy signatures are not accepted).

`statement.json` and `statement.json.minisig` are a key statement made the way the owner makes one (`packaging/keys/README.md`): the body file written by hand, then `minisign -S -m statement.json -t 'lodestar key statement'` with the recovery key (here the tool key stands in for it).
