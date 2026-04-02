"""
Tests: dynamic node-ID allocation via uavcan.pnp.NodeIDAllocationData.1.0

These tests assume the DUT boots anonymous and becomes operational only after
an allocator assigns a node ID.
"""
from __future__ import annotations

import asyncio

import pytest
import pycyphal.application
import pycyphal.presentation

import uavcan.node.ExecuteCommand_1_3 as EC
import uavcan.node.GetInfo_1_0 as GI

from .conftest import wait_for_heartbeat

STATUS_SUCCESS = EC.Response.STATUS_SUCCESS
CMD_RESTART = EC.Request.COMMAND_RESTART


async def test_pnp_allocator_assigns_expected_node_id(
    test_node: pycyphal.application.Node,
    dut_node_id: int,
    gi_client: pycyphal.presentation.Client,
    pnp_allocator,
) -> None:
    """The DUT should become reachable on the allocated node ID."""
    allocated = await pnp_allocator.wait_allocated(timeout=30.0)
    assert allocated, f"PnP allocator never responded to a request within 30 s"

    hb = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert hb is not None, f"No heartbeat from dynamically allocated node {dut_node_id}"

    result = await gi_client.call(GI.Request())
    assert result is not None, "GetInfo timed out after node-ID allocation"


@pytest.mark.destructive
async def test_restart_before_allocator_then_allocate_when_allocator_appears(
    ec_client: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    dut_node_id: int,
    pnp_allocator,
) -> None:
    """
    If the node reboots while allocator is offline, it should stay non-operational
    and then come online once allocator is started again.
    """
    result = await ec_client.call(EC.Request(command=CMD_RESTART, parameter=[]))
    assert result is not None, "ExecuteCommand(RESTART) timed out"
    response, _ = result
    response_status = getattr(response, "status", None)
    assert response_status == STATUS_SUCCESS, (
        f"Expected STATUS_SUCCESS on restart command, got {response_status}"
    )

    await pnp_allocator.stop()
    await asyncio.sleep(1.0)

    hb_without_allocator = await wait_for_heartbeat(test_node, dut_node_id, timeout=3.0)
    assert hb_without_allocator is None, (
        "DUT became operational while allocator was offline; expected anonymous retry behavior"
    )

    await pnp_allocator.start()
    allocated = await pnp_allocator.wait_allocated(timeout=30.0)
    assert allocated, "PnP allocator never responded to a request after restart within 30 s"

    hb_after_allocator = await wait_for_heartbeat(test_node, dut_node_id, timeout=5.0)
    assert hb_after_allocator is not None, (
        "DUT did not become operational after allocator was started"
    )
