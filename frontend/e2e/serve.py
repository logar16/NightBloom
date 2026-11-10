"""Run the isolated E2E server without ever contacting a live Aladin/SAMP hub."""
import pyvo.samp


def disabled_samp(*args, **kwargs):
    raise RuntimeError("Live SAMP is disabled in E2E tests.")


pyvo.samp.SAMPIntegratedClient = disabled_samp

from astroimp.cli import main

main()
