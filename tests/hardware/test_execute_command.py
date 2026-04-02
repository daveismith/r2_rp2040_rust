"""
Tests: uavcan.node.ExecuteCommand (service ID 435, version 1.3)

Tests are ordered from safest to most disruptive:
  1. Non-destructive commands first (IDENTIFY, BAD_COMMAND)
  2. Destructive commands last (RESTART, FACTORY_RESET)

Mark groups:
  - @pytest.mark.destructive  →  run with '-m not destructive' to skip reboots

Usage:
    # All tests (including reboots):
    .venv/bin/pytest tests/hardware/test_execute_command.py -v

    # Skip reboots:
    .venv/bin/pytest tests/hardware/test_execute_command.py -v -m "not destructive"
"""
from __future__ import annotations

import asyncio
import pytest
import pycyphal.application
import pycyphal.presentation

import uavcan.node.ExecuteCommand_1_3 as EC

from .conftest import wait_for_heartbeat

# ---------------------------------------------------------------------------
# Constants imported from the compiled DSDL types
# ---------------------------------------------------------------------------
CMD_IDENTIFY = EC.Request.COMMAND_IDENTIFY             # 65529
CMD_RESTART = EC.Request.COMMAND_RESTART               # 65535
CMD_FACTORY_RESET = EC.Request.COMMAND_FACTORY_RESET   # 65532
CMD_BEGIN_SW_UPDATE = EC.Request.COMMAND_BEGIN_SOFTWARE_UPDATE  # 65533

STATUS_SUCCESS = EC.Response.STATUS_SUCCESS            # 0
STATUS_BAD_COMMAND = EC.Response.STATUS_BAD_COMMAND    # 3
STATUS_BAD_STATE = EC.Response.STATUS_BAD_STATE        # 5
STATUS_BAD_PARAMETER = EC.Response.STATUS_BAD_PARAMETER  # 4

# Timeout to wait for the node to reboot and send a heartbeat (seconds).
REBOOT_TIMEOUT = 10.0


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

async def _call(client: pycyphal.presentation.Client, command: int, params: bytes = b""):
    """Send an ExecuteCommand request and return (response, transfer) or None."""
    req = EC.Request(command=command, parameter=list(params))
    return await client.call(req)


# ---------------------------------------------------------------------------
# Non-destructive tests
# ---------------------------------------------------------------------------


async def test_identify_returns_success(ec_client: pycyphal.presentation.Client) -> None:
    """COMMAND_IDENTIFY (65529) must return STATUS_SUCCESS."""
    result = await _call(ec_client, CMD_IDENTIFY)
    assert result is not None, "ExecuteCommand(IDENTIFY) timed out — is the node running?"
    response, _ = result
    assert response.status == STATUS_SUCCESS, (
        f"Expected STATUS_SUCCESS (0), got {response.status}"
    )


async def test_identify_idempotent(ec_client: pycyphal.presentation.Client) -> None:
    """Calling COMMAND_IDENTIFY twice rapidly both return STATUS_SUCCESS."""
    for i in range(2):
        result = await _call(ec_client, CMD_IDENTIFY)
        assert result is not None, f"Call {i+1}: ExecuteCommand(IDENTIFY) timed out"
        response, _ = result
        assert response.status == STATUS_SUCCESS, (
            f"Call {i+1}: Expected STATUS_SUCCESS, got {response.status}"
        )
        await asyncio.sleep(0.2)


async def test_bad_command_returns_bad_command(ec_client: pycyphal.presentation.Client) -> None:
    """An unknown vendor command (0x1234) must return STATUS_BAD_COMMAND."""
    result = await _call(ec_client, 0x1234)
    assert result is not None, "ExecuteCommand(0x1234) timed out"
    response, _ = result
    assert response.status == STATUS_BAD_COMMAND, (
        f"Expected STATUS_BAD_COMMAND (3), got {response.status}"
    )


async def test_bad_command_zero_returns_bad_command(ec_client: pycyphal.presentation.Client) -> None:
    """Command 0x0000 (not implemented) must return STATUS_BAD_COMMAND."""
    result = await _call(ec_client, 0x0000)
    assert result is not None, "ExecuteCommand(0x0000) timed out"
    response, _ = result
    assert response.status == STATUS_BAD_COMMAND, (
        f"Expected STATUS_BAD_COMMAND (3), got {response.status}"
    )


async def test_begin_software_update_rejects_empty_parameter(
    ec_client: pycyphal.presentation.Client,
) -> None:
    """
    COMMAND_BEGIN_SOFTWARE_UPDATE (65533) requires a non-empty file path.
    Keep this check in the non-OTA suite so we don't start a real update here.
    """
    result = await _call(ec_client, CMD_BEGIN_SW_UPDATE, b"")
    assert result is not None, "ExecuteCommand(BEGIN_SOFTWARE_UPDATE) timed out"
    response, _ = result
    assert response.status == STATUS_BAD_PARAMETER, (
        f"Expected STATUS_BAD_PARAMETER (4), got {response.status}"
    )


# ---------------------------------------------------------------------------
# Destructive tests — reboot the node
# ---------------------------------------------------------------------------


@pytest.mark.destructive
async def test_restart_returns_success_and_node_reappears(
    ec_client: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    dut_node_id: int,
) -> None:
    """
    COMMAND_RESTART (65535):
      1. Response must be STATUS_SUCCESS.
      2. Node must reappear (send a Heartbeat) within REBOOT_TIMEOUT seconds.
    """
    result = await _call(ec_client, CMD_RESTART)
    assert result is not None, "ExecuteCommand(RESTART) timed out — no response received"
    response, _ = result
    assert response.status == STATUS_SUCCESS, (
        f"Expected STATUS_SUCCESS, got {response.status}"
    )

    # Give the node a moment to actually start rebooting before we listen.
    await asyncio.sleep(0.5)

    hb = await wait_for_heartbeat(test_node, dut_node_id, timeout=REBOOT_TIMEOUT)
    assert hb is not None, (
        f"Node {dut_node_id} did not reappear within {REBOOT_TIMEOUT} s after RESTART. "
        "Check serial log for panic or boot failure."
    )


@pytest.mark.destructive
async def test_factory_reset_returns_success_and_node_reappears(
    ec_client: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    dut_node_id: int,
) -> None:
    """
    COMMAND_FACTORY_RESET (65532):
      1. Response must be STATUS_SUCCESS.
         Note: the firmware sends the full multi-frame response *before* erasing
         flash, so the response must arrive even though erasing takes ~6 seconds.
      2. Node must reappear (send a Heartbeat) within REBOOT_TIMEOUT seconds
         after the response is received.

    Implementation note: "factory_reset" in the response text produces a 3-frame
    CAN transfer.  The firmware intentionally flushes all TX frames before
    calling blocking_erase() to avoid a timing bug where the erase interrupts
    in-progress frame transmission.
    """
    result = await _call(ec_client, CMD_FACTORY_RESET)
    assert result is not None, (
        "ExecuteCommand(FACTORY_RESET) timed out — no response received. "
        "This may indicate the TX flush bug: the firmware must flush all CAN frames "
        "before calling blocking_erase()."
    )
    response, _ = result
    assert response.status == STATUS_SUCCESS, (
        f"Expected STATUS_SUCCESS, got {response.status}"
    )

    # Flash erase can take several seconds; add headroom to the heartbeat wait.
    erase_headroom = 8.0
    await asyncio.sleep(0.5)

    hb = await wait_for_heartbeat(
        test_node, dut_node_id, timeout=REBOOT_TIMEOUT + erase_headroom
    )
    assert hb is not None, (
        f"Node {dut_node_id} did not reappear within {REBOOT_TIMEOUT + erase_headroom} s "
        "after FACTORY_RESET. Check serial log."
    )
