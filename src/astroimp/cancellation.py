"""Cooperative checkpoints shared by background workers."""


class CancelledError(Exception):
    pass


def check_cancelled(progress) -> None:
    check = getattr(progress, "check_cancelled", None)
    if check:
        check()
