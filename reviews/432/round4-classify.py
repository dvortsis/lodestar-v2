import re, collections, sys
Q = r'("(?:[^"\\]|\\.)*")'
for f in sys.argv[1:]:
    cnt = collections.Counter()
    for line in open(f):
        if not line.startswith('REG'):
            continue
        if line.startswith('REG scrub'):
            cnt['scrub()'] += 1
            continue
        m = re.match(r'REG case=(\d+) a=' + Q + ' b=' + Q, line)
        case = int(m.group(1)); a = eval(m.group(2)); b = eval(m.group(3))
        if case in (5, 6):
            cnt['passkey-path'] += 1
        elif '://' in b:
            cnt['accepted(:// after secret)'] += 1
        elif '://' in a:
            cnt['://-before-secret'] += 1
        elif '\n' in a + b:
            cnt['raw-newline'] += 1
        else:
            cnt['other'] += 1
    print(f, sum(cnt.values()), dict(cnt))
