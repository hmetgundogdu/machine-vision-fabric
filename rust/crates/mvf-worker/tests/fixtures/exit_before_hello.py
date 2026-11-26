"""Fails during import: exits with code 3 before the handshake."""
import sys

sys.stderr.write("fixture: failing before hello\n")
sys.exit(3)
