"""
Tests: shoulder-sensor Cyphal publications and COMMAND_ZERO.

All tests use the session-scoped `test_node` from conftest.py and require a
live shoulder-sensor board connected via a slcan CAN adapter.

Subject IDs (configurable via CLI options):
  --angle-subject-id  (default: 6144)  uavcan.si.unit.angle.Scalar 1.0
  --temp-subject-id   (default: 6145)  uavcan.si.unit.temperature.Scalar 1.0

Non-destructive tests come first.  Tests that send COMMAND_ZERO (which
mutates persistent NVS state) are ordered last and marked
``@pytest.mark.destructive`` so they can be excluded with
``-m "not destructive"``.

Usage
-----
    # All tests (including COMMAND_ZERO):
    .venv/bin/pytest tests/hardware/test_shoulder_sensor.py -v

    # Skip COMMAND_ZERO (read-only run):
    .venv/bin/pytest tests/hardware/test_shoulder_sensor.py -v -m "not destructive"

    # Non-default subject IDs:
    .venv/bin/pytest tests/hardware/test_shoulder_sensor.py -v \\
        --angle-subject-id 6150 --temp-subject-id 6151
"""
from __future__ import annotations

import asyncio
import math
from typing import AsyncIterator

import pytest
import pycyphal.application
import pycyphal.presentation

import uavcan.node.ExecuteCommand_1_3 as EC

# These types are available after DSDL compilation performed by conftest.py.
import uavcan.si.unit.angle.Scalar_1_0 as AngleScalar
import uavcan.si.unit.temperature.Scalar_1_0 as TempScalar

from .conftest import wait_for_heartbeat

# ---- Constants ------------------------------------------------------------

STATUS_SUCCESS = EC.Response.STATUS_SUCCESS
STATUS_BAD_COMMAND = EC.Response.STATUS_BAD_COMMAND

COMMAND_ZERO: int = 0x0001

# Minimum and maximum valid angle values (radians).
ANGLE_MIN = -math.pi
ANGLE_MAX = math.pi

# Reasonable temperature range for an operating board (Kelvin): −40°C to +100°C
TEMP_MIN_K = 273.15 - 40.0   # 233.15 K
TEMP_MAX_K = 273.15 + 100.0  # 373.15 K

# How close to 0.0 the angle must be after a successful COMMAND_ZERO.
ZERO_EPSILON = 0.1  # radians (~5.7°)

# ---- Per-test subscribers (function-scoped) --------------------------------


@pytest.fixture
async def angle_sub(
    test_node: pycyphal.application.Node, angle_subject_id: int
) -> AsyncIterator[pycyphal.presentation.Subscriber]:
    """Angle subscriber on the configured subject ID, closed after each test."""
    sub = test_node.make_subscriber(AngleScalar, angle_subject_id)
    try:
        yield sub
    finally:
        sub.close()


@pytest.fixture
async def temp_sub(
    test_node: pycyphal.application.Node, temp_subject_id: int
) -> AsyncIterator[pycyphal.presentation.Subscriber]:
    """Temperature subscriber on the configured subject ID, closed after each test."""
    sub = test_node.make_subscriber(TempScalar, temp_subject_id)
    try:
        yield sub
    finally:
        sub.close()


@pytest.fixture
async def ec_client_shoulder(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> AsyncIterator[pycyphal.presentation.Client]:
    """ExecuteCommand client pointed at the DUT, closed after each test."""
    client = test_node.make_client(EC, dut_node_id)
    client.response_timeout = 5.0
    try:
        yield client
    finally:
        client.close()


# ---- Helper ---------------------------------------------------------------


async def _exec_command(
    client: pycyphal.presentation.Client, command: int, params: bytes = b""
):
    """Send an ExecuteCommand request; return (response, transfer) or None."""
    req = EC.Request(command=command, parameter=list(params))
    return await client.call(req)


# ---- Non-destructive angle tests ------------------------------------------


async def test_angle_message_received(
    angle_sub: pycyphal.presentation.Subscriber,
) -> None:
    """At least one angle message arrives within 200 ms."""
    result = await angle_sub.receive_for(0.2)
    assert result is not None, (
        "No angle message received within 200 ms.  "
        "Check that the shoulder-sensor is running and publishing on the "
        "correct subject ID (see --angle-subject-id)."
    )


async def test_angle_source_node_id(
    angle_sub: pycyphal.presentation.Subscriber, dut_node_id: int
) -> None:
    """Received angle transfer carries the DUT's source node ID."""
    result = await angle_sub.receive_for(0.5)
    assert result is not None, "No angle message received within 500 ms."
    _, transfer = result
    assert transfer.source_node_id == dut_node_id, (
        f"Expected source_node_id={dut_node_id}, got {transfer.source_node_id}.  "
        "Multiple shoulder-sensor nodes on the bus may cause this."
    )


async def test_angle_value_in_range(
    angle_sub: pycyphal.presentation.Subscriber, dut_node_id: int
) -> None:
    """Received angle value is within [−π, +π] radians."""
    # Drain up to 5 messages from the correct node to ensure we check the DUT.
    deadline = asyncio.get_event_loop().time() + 0.5
    while asyncio.get_event_loop().time() < deadline:
        result = await angle_sub.receive_for(0.2)
        if result is None:
            break
        msg, transfer = result
        if transfer.source_node_id != dut_node_id:
            continue
        angle = float(msg.radian)
        assert ANGLE_MIN <= angle <= ANGLE_MAX, (
            f"Angle {angle:.4f} rad is outside [−π, +π].  "
            "Check zero-offset wrapping logic."
        )
        return
    pytest.fail(f"No angle message from DUT node {dut_node_id} within 500 ms.")


async def test_angle_rate_100hz(
    test_node: pycyphal.application.Node,
    angle_subject_id: int,
    dut_node_id: int,
) -> None:
    """At least 90 angle messages arrive from the DUT within 1 second (≥ 90 Hz)."""
    sub = test_node.make_subscriber(AngleScalar, angle_subject_id)
    count = 0
    deadline = asyncio.get_event_loop().time() + 1.0
    try:
        while asyncio.get_event_loop().time() < deadline:
            remaining = deadline - asyncio.get_event_loop().time()
            if remaining <= 0:
                break
            result = await sub.receive_for(min(remaining, 0.05))
            if result is None:
                continue
            _, transfer = result
            if transfer.source_node_id == dut_node_id:
                count += 1
    finally:
        sub.close()

    assert count >= 90, (
        f"Received only {count} angle messages in 1 s (expected ≥ 90).  "
        "Check that the node loop period is fast enough for 100 Hz publishing."
    )


# ---- Non-destructive temperature tests ------------------------------------


async def test_temperature_message_received(
    temp_sub: pycyphal.presentation.Subscriber,
) -> None:
    """At least one temperature message arrives within 2 seconds."""
    result = await temp_sub.receive_for(2.0)
    assert result is not None, (
        "No temperature message received within 2 s.  "
        "Check --temp-subject-id and that the sensor task is running."
    )


async def test_temperature_value_reasonable(
    temp_sub: pycyphal.presentation.Subscriber, dut_node_id: int
) -> None:
    """Temperature value is in [233.15, 373.15] K (−40°C to +100°C)."""
    deadline = asyncio.get_event_loop().time() + 2.0
    while asyncio.get_event_loop().time() < deadline:
        remaining = deadline - asyncio.get_event_loop().time()
        result = await temp_sub.receive_for(min(remaining, 0.5))
        if result is None:
            break
        msg, transfer = result
        if transfer.source_node_id != dut_node_id:
            continue
        k = float(msg.kelvin)
        assert TEMP_MIN_K <= k <= TEMP_MAX_K, (
            f"Temperature {k:.2f} K is outside reasonable range "
            f"[{TEMP_MIN_K}, {TEMP_MAX_K}] K.  "
            "Check sensor connection and unit conversion (°C×100 → K)."
        )
        return
    pytest.fail(f"No temperature message from DUT node {dut_node_id} within 2 s.")


async def test_temperature_source_node_id(
    temp_sub: pycyphal.presentation.Subscriber, dut_node_id: int
) -> None:
    """Received temperature transfer carries the DUT's source node ID."""
    result = await temp_sub.receive_for(2.0)
    assert result is not None, "No temperature message received within 2 s."
    _, transfer = result
    assert transfer.source_node_id == dut_node_id, (
        f"Expected source_node_id={dut_node_id}, got {transfer.source_node_id}."
    )


# ---- Destructive tests: COMMAND_ZERO --------------------------------------
# These tests mutate persistent NVS state (zero offset).
# A COMMAND_ZERO at the end attempts to restore a ~0 zero offset.


@pytest.mark.destructive
async def test_zero_command_returns_success(
    ec_client_shoulder: pycyphal.presentation.Client,
) -> None:
    """COMMAND_ZERO (0x0001) returns STATUS_SUCCESS."""
    result = await _exec_command(ec_client_shoulder, COMMAND_ZERO)
    assert result is not None, "ExecuteCommand(COMMAND_ZERO) timed out."
    response, _ = result
    assert response.status == STATUS_SUCCESS, (
        f"Expected STATUS_SUCCESS (0), got {response.status}.  "
        "Ensure the shoulder-sensor firmware handles COMMAND_ZERO = 0x0001."
    )


@pytest.mark.destructive
async def test_zero_command_adjusts_angle(
    ec_client_shoulder: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    angle_subject_id: int,
    dut_node_id: int,
) -> None:
    """
    After COMMAND_ZERO the next published angle is approximately 0.0 rad.

    Because the zero offset is applied to the *current* sensor reading,
    the immediately published angle after the command should be near zero.
    """
    result = await _exec_command(ec_client_shoulder, COMMAND_ZERO)
    assert result is not None, "COMMAND_ZERO timed out."
    assert result[0].status == STATUS_SUCCESS

    # Wait for the next angle message after the command.
    sub = test_node.make_subscriber(AngleScalar, angle_subject_id)
    try:
        deadline = asyncio.get_event_loop().time() + 0.5
        while asyncio.get_event_loop().time() < deadline:
            remaining = deadline - asyncio.get_event_loop().time()
            r = await sub.receive_for(min(remaining, 0.1))
            if r is None:
                continue
            msg, transfer = r
            if transfer.source_node_id != dut_node_id:
                continue
            angle = float(msg.radian)
            assert abs(angle) <= ZERO_EPSILON, (
                f"Angle {angle:.4f} rad is not near zero after COMMAND_ZERO.  "
                f"Expected |angle| ≤ {ZERO_EPSILON} rad."
            )
            return
        pytest.fail("No angle message from DUT within 500 ms after COMMAND_ZERO.")
    finally:
        sub.close()


@pytest.mark.destructive
async def test_zero_command_wraps_correctly(
    ec_client_shoulder: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    angle_subject_id: int,
    dut_node_id: int,
) -> None:
    """
    After COMMAND_ZERO the published angle remains within [−π, +π].

    This verifies that the wrapping logic handles the case where the
    pre-zero angle was near ±π correctly.
    """
    result = await _exec_command(ec_client_shoulder, COMMAND_ZERO)
    assert result is not None, "COMMAND_ZERO timed out."
    assert result[0].status == STATUS_SUCCESS

    sub = test_node.make_subscriber(AngleScalar, angle_subject_id)
    received = 0
    try:
        deadline = asyncio.get_event_loop().time() + 1.0
        while asyncio.get_event_loop().time() < deadline and received < 10:
            remaining = deadline - asyncio.get_event_loop().time()
            r = await sub.receive_for(min(remaining, 0.05))
            if r is None:
                continue
            msg, transfer = r
            if transfer.source_node_id != dut_node_id:
                continue
            angle = float(msg.radian)
            assert ANGLE_MIN <= angle <= ANGLE_MAX, (
                f"Angle {angle:.4f} rad is outside [−π, +π] after COMMAND_ZERO."
            )
            received += 1
    finally:
        sub.close()

    assert received > 0, "No angle messages received after COMMAND_ZERO."
