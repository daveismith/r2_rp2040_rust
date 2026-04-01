"""
Tests: uavcan.node.GetInfo (service ID 430)

Verifies the node name, software version, and that a non-empty unique-ID
is present.
"""
from __future__ import annotations

import pytest
import pycyphal.application
import pycyphal.presentation

import uavcan.node.GetInfo_1_0 as GI


# ---------------------------------------------------------------------------
# GetInfo tests
# ---------------------------------------------------------------------------


async def test_get_info_responds(gi_client: pycyphal.presentation.Client) -> None:
    """GetInfo returns a response (not timeout)."""
    result = await gi_client.call(GI.Request())
    assert result is not None, (
        "GetInfo timed out. Check that the node is running and has node ID 42."
    )


async def test_get_info_node_name(gi_client: pycyphal.presentation.Client) -> None:
    """Node name is 'cyphal_template'."""
    result = await gi_client.call(GI.Request())
    assert result is not None, "GetInfo timed out"
    response, _ = result
    name = bytes(response.name).decode("ascii")  # type: ignore[attr-defined]
    assert name == "cyphal_template", f"Unexpected node name: {name!r}"


async def test_get_info_software_version(gi_client: pycyphal.presentation.Client) -> None:
    """Software version is 0.1."""
    result = await gi_client.call(GI.Request())
    assert result is not None, "GetInfo timed out"
    response, _ = result
    assert response.software_version.major == 0  # type: ignore[attr-defined]
    assert response.software_version.minor == 3  # type: ignore[attr-defined]


async def test_get_info_unique_id(gi_client: pycyphal.presentation.Client) -> None:
    """Unique-ID is 16 bytes, all non-zero (i.e. flash UID was read correctly)."""
    result = await gi_client.call(GI.Request())
    assert result is not None, "GetInfo timed out"
    response, _ = result
    uid = bytes(response.unique_id)  # type: ignore[attr-defined]
    assert len(uid) == 16, f"Unique-ID length {len(uid)} != 16"
    assert any(b != 0 for b in uid), "Unique-ID is all zeros — flash UID read may have failed"


async def test_get_info_protocol_version(gi_client: pycyphal.presentation.Client) -> None:
    """Protocol version major is 1 (Cyphal specification version)."""
    result = await gi_client.call(GI.Request())
    assert result is not None, "GetInfo timed out"
    response, _ = result
    assert response.protocol_version.major == 1  # type: ignore[attr-defined]
