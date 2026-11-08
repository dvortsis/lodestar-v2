"""UTC timestamps as `YYYY-MM-DDTHH:MM:SSZ`; nothing else is accepted."""

from datetime import datetime, timezone

_FMT = "%Y-%m-%dT%H:%M:%SZ"


def fmt(t: datetime) -> str:
    return t.astimezone(timezone.utc).strftime(_FMT)


def parse_ts(s: str) -> datetime:
    if not isinstance(s, str):
        raise ValueError("timestamp must be a string")
    return datetime.strptime(s, _FMT).replace(tzinfo=timezone.utc)
