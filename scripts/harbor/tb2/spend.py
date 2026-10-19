"""What the calls in the relay's meters were charged, and whether a budget is spent.

    python3 spend.py METER [METER ...] [--since EPOCH|ISO] [--budget USD] [--credit]

A call's charge is what the upstream said it cost: the "cost" the relay
read from a response header, or else a "cost" in the usage it reported.
Nothing is estimated from token counts, which undercount a gateway's own
charges; a call answered with usage but no charge is counted apart, so a
short total shows. A meter not yet written counts as $0. With --budget it
exits 3 once the total reaches the budget, and with --credit it exits 4
when the call that began last was refused with 402, as a key out of credit
refuses every call.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from datetime import datetime, timezone
from pathlib import Path


def moment(text: str) -> float:
    """Seconds since the epoch, as given or from an ISO time, read as UTC when it names no zone."""
    try:
        return float(text)
    except ValueError:
        when = datetime.fromisoformat(text)
    return (when if when.tzinfo else when.replace(tzinfo=timezone.utc)).timestamp()


def budget(text: str) -> float:
    """A budget in dollars, which no total reaches if it is NaN or infinite, so it would stop nothing."""
    try:
        amount = float(text)
    except ValueError:
        amount = math.nan
    if not math.isfinite(amount):
        raise argparse.ArgumentTypeError(f"not a number of dollars: {text!r}")
    return amount


def charge(row: dict) -> float | None:
    usage = row.get("usage")
    for cost in (row.get("cost"), usage.get("cost") if isinstance(usage, dict) else None):
        if isinstance(cost, (int, float)) and not isinstance(cost, bool):
            return float(cost)
    return None


def calls(meter: Path, since: float = 0) -> list[dict]:
    """The POST calls in `meter` from `since` on."""
    try:
        lines = meter.read_text(errors="replace").splitlines()
    except FileNotFoundError:
        return []
    found = []
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            continue  # the line the relay is writing
        if isinstance(row, dict) and row.get("method") == "POST" and (row.get("t") or 0) >= since:
            found.append(row)
    return found


def tally(meter: Path, since: float = 0) -> tuple[float, int, int]:
    """Dollars charged in `meter` from `since` on, the calls charged, and those answered without a charge."""
    dollars, charged, uncharged = 0.0, 0, 0
    for row in calls(meter, since):
        cost = charge(row)
        if cost is not None:
            dollars += cost
            charged += 1
        elif row.get("usage") and (row.get("status") or 0) < 400:
            uncharged += 1
    return dollars, charged, uncharged


def out_of_credit(meters: list[Path], since: float = 0) -> bool:
    """Whether the call that began last in `meters`, from `since` on, was refused with 402.

    By when they began, as a stream begun before the key ran dry can end
    well after the refusals that followed it.
    """
    rows = [row for meter in meters for row in calls(meter, since)]
    last = max(rows, key=lambda r: float(r.get("t") or 0), default=None)
    return last is not None and last.get("status") == 402


def said(dollars: float, charged: int, uncharged: int) -> str:
    line = f"${dollars:.2f} over {charged} call{'' if charged == 1 else 's'}"
    if uncharged:
        line += f", {uncharged} call{'' if uncharged == 1 else 's'} without a charge"
    return line


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("meters", nargs="+")
    p.add_argument("--since", type=moment, default=0, help="count calls from then on: epoch seconds or ISO")
    p.add_argument("--budget", type=budget, help="exit 3 once the total reaches this many dollars")
    p.add_argument("--credit", action="store_true", help="exit 4 when the newest call was refused with 402")
    a = p.parse_args()
    total = [0.0, 0, 0]
    for meter in a.meters:
        try:
            counts = tally(Path(meter), a.since)
        except OSError as err:
            sys.exit(f"spend.py: cannot read the meter {meter}: {err.strerror}")
        total = [t + c for t, c in zip(total, counts)]
        print(f"{meter}: {said(*counts)}")
    if len(a.meters) > 1:
        print(f"In all: {said(*total)}")
    if a.budget is not None and total[0] >= a.budget:
        print(f"The budget of ${a.budget:.2f} is spent.")
        sys.exit(3)
    if a.credit and out_of_credit([Path(m) for m in a.meters], a.since):
        print("The newest call was refused with 402: the key is out of credit.")
        sys.exit(4)


if __name__ == "__main__":
    main()
