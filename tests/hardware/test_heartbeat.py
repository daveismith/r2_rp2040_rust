"""
Tests: uavcan.node.Heartbeat (subject ID 7509)

The DUT should publish a heartbeat every ~1 second.  We wait up to 5 seconds
to receive one and verify the source node ID.
"""
from __future__ import annotations

import asyncio
import pytest
import pycyphal.application
import pycyphal.transport

import uavcan.node.Heartbeat_1_0 as HB
import uavcan.node.Health_1_0 as Health
import uavcan.node.Mode_1_0 as Mode

from .conftest import wait_for_heartbeat

# ---------------------------------------------------------------------------
# Heartbeat tests
# ---------------------------------------------------------------------------


async def test_heartbeat_received(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> None:
    """DUT publishes a Heartbeat within 5 seconds of test start."""
    msg = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert msg is not None, (
        f"No Heartbeat received from node {dut_node_id} within 5 s. "
        "Check CAN adapter, bitrate, and node ID."
    )


async def test_heartbeat_health_nominal(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> None:
    """The Heartbeat reports HEALTH_NOMINAL (0)."""
    msg = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert msg is not None, f"No Heartbeat from node {dut_node_id}"
    assert int(msg.health.value) == int(Health.NOMINAL), (
        f"Expected HEALTH_NOMINAL (0), got {msg.health.value}"
    )


async def test_heartbeat_mode_operational(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> None:
    """The Heartbeat reports MODE_OPERATIONAL (0)."""
    msg = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert msg is not None, f"No Heartbeat from node {dut_node_id}"
    assert int(msg.mode.value) == int(Mode.OPERATIONAL), (
        f"Expected MODE_OPERATIONAL (0), got {msg.mode.value}"
    )



async def test_two_consecutive_heartbeats(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> None:
    """Two consecutive Heartbeats arrive within 3 seconds of each other (uptime must increase)."""
    first = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert first is not None, f"No first Heartbeat from node {dut_node_id}"

    second = await wait_for_heartbeat(test_node, dut_node_id, timeout=3.0)
    assert second is not None, f"No second Heartbeat from node {dut_node_id}"

    assert second.uptime >= first.uptime, (
        f"Uptime went backwards: {first.uptime} → {second.uptime}"
    )
