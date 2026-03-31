"""
pytest conftest for Cyphal hardware-in-the-loop tests.

All tests in this directory talk to a live RP2040 node over a slcan CAN adapter.

Quick start
-----------
    .venv/bin/pytest tests/hardware/ -v

With non-default adapter or node:
    .venv/bin/pytest tests/hardware/ -v \\
        --can-iface slcan:/dev/cu.usbmodem1101 \\
        --can-bitrate 1000000 \\
        --dut-node-id 42

Skip destructive tests (reboot / factory-reset):
    .venv/bin/pytest tests/hardware/ -v -m "not destructive"
"""
from __future__ import annotations

import pathlib
import sys

# ---------------------------------------------------------------------------
# DSDL compilation — MUST happen before any import of pycyphal.application
# because pycyphal.application._node imports uavcan.node at module load time.
# ---------------------------------------------------------------------------

_WORKSPACE = pathlib.Path(__file__).parents[2]
_DSDL_OUT = _WORKSPACE / ".dsdl_compiled"
_DSDL_UAVCAN = _WORKSPACE / "public_regulated_data_types" / "uavcan"
_DSDL_REG = _WORKSPACE / "public_regulated_data_types" / "reg"


def _ensure_dsdl_compiled() -> None:
    """Compile DSDL to .dsdl_compiled/ once; reuse on subsequent runs."""
    if not (_DSDL_OUT / "uavcan").exists():
        import pycyphal.dsdl

        _DSDL_OUT.mkdir(parents=True, exist_ok=True)
        pycyphal.dsdl.compile_all([_DSDL_UAVCAN, _DSDL_REG], _DSDL_OUT)

    if str(_DSDL_OUT) not in sys.path:
        sys.path.insert(0, str(_DSDL_OUT))


_ensure_dsdl_compiled()

# ---------------------------------------------------------------------------
# Normal imports (uavcan is now importable)
# ---------------------------------------------------------------------------

import asyncio
from typing import AsyncIterator

import pytest
import pycyphal.application
import pycyphal.transport.can
import pycyphal.transport.can.media.pythoncan
import uavcan.node
import uavcan.node.ExecuteCommand_1_3 as EC
import uavcan.node.GetInfo_1_0 as GI
import uavcan.node.Heartbeat_1_0 as HB

# ---------------------------------------------------------------------------
# Custom markers
# ---------------------------------------------------------------------------


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "destructive: marks tests that reboot or erase the DUT "
        "(deselect with '-m not destructive')",
    )
    config.addinivalue_line(
        "markers",
        "ota: marks OTA firmware update tests",
    )


# ---------------------------------------------------------------------------
# CLI options
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--can-iface",
        default="slcan:/dev/cu.usbmodem1101",
        help="python-can interface string (default: slcan:/dev/cu.usbmodem1101). "
        "Format: <backend>:<channel>[@<serial-baudrate>]. "
        "Example: slcan:/dev/cu.usbmodem1101@115200",
    )
    parser.addoption(
        "--can-bitrate",
        default=1_000_000,
        type=int,
        help="CAN bus bitrate in bits/s (default: 1000000)",
    )
    parser.addoption(
        "--dut-node-id",
        default=42,
        type=int,
        help="Cyphal node ID of the device under test (default: 42)",
    )
    parser.addoption(
        "--local-node-id",
        default=10,
        type=int,
        help="Cyphal node ID used by the test harness (default: 247)",
    )
    parser.addoption(
        "--run-ota",
        action="store_true",
        default=False,
        help="Run destructive OTA update tests (default: disabled)",
    )
    parser.addoption(
        "--ota-file-path",
        default="/fw/cyphal-template.bin",
        help="Path sent in COMMAND_BEGIN_SOFTWARE_UPDATE parameter",
    )
    parser.addoption(
        "--ota-image-file",
        default="target/thumbv6m-none-eabi/debug/cyphal-template.bin",
        help="Local firmware image served by test file.Read server",
    )


# ---------------------------------------------------------------------------
# Session-scoped primitive fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def can_iface(request: pytest.FixtureRequest) -> str:
    return request.config.getoption("--can-iface")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def can_bitrate(request: pytest.FixtureRequest) -> int:
    return request.config.getoption("--can-bitrate")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def dut_node_id(request: pytest.FixtureRequest) -> int:
    return request.config.getoption("--dut-node-id")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def local_node_id(request: pytest.FixtureRequest) -> int:
    return request.config.getoption("--local-node-id")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def run_ota(request: pytest.FixtureRequest) -> bool:
    return request.config.getoption("--run-ota")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def ota_file_path(request: pytest.FixtureRequest) -> str:
    return request.config.getoption("--ota-file-path")  # type: ignore[return-value]


@pytest.fixture(scope="session")
def ota_image_file(request: pytest.FixtureRequest) -> str:
    return request.config.getoption("--ota-image-file")  # type: ignore[return-value]


# ---------------------------------------------------------------------------
# Session-scoped pycyphal node
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
async def test_node(
    can_iface: str, can_bitrate: int, local_node_id: int
) -> AsyncIterator[pycyphal.application.Node]:
    """
    Open the CAN adapter and create a pycyphal node for the duration of the
    test session.  The node is started before the first test and closed after
    the last.

    Shared by all tests — do NOT close it inside a test.
    """
    media = pycyphal.transport.can.media.pythoncan.PythonCANMedia(
        can_iface,
        bitrate=can_bitrate,
        mtu=8,
    )
    transport = pycyphal.transport.can.CANTransport(media, local_node_id=local_node_id)
    info = pycyphal.application.NodeInfo(name="pytest.cyphal.harness")
    node = pycyphal.application.make_node(info, transport=transport)
    node.start()
    try:
        yield node
    finally:
        node.close()


# ---------------------------------------------------------------------------
# Function-scoped service clients
# ---------------------------------------------------------------------------


@pytest.fixture
async def ec_client(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> AsyncIterator[pycyphal.presentation.Client]:
    """ExecuteCommand 1.3 client, closed after each test."""
    client = test_node.make_client(EC, dut_node_id)
    client.response_timeout = 5.0
    try:
        yield client
    finally:
        client.close()


@pytest.fixture
async def gi_client(
    test_node: pycyphal.application.Node, dut_node_id: int
) -> AsyncIterator[pycyphal.presentation.Client]:
    """GetInfo 1.0 client, closed after each test."""
    client = test_node.make_client(GI, dut_node_id)
    client.response_timeout = 5.0
    try:
        yield client
    finally:
        client.close()


@pytest.fixture
async def heartbeat_sub(
    test_node: pycyphal.application.Node,
) -> AsyncIterator[pycyphal.presentation.Subscriber]:
    """Heartbeat 1.0 subscriber, closed after each test."""
    sub = test_node.make_subscriber(HB)
    try:
        yield sub
    finally:
        sub.close()


# ---------------------------------------------------------------------------
# Helper: wait for a heartbeat from a specific node
# ---------------------------------------------------------------------------


async def wait_for_heartbeat(
    test_node: pycyphal.application.Node,
    source_node_id: int,
    timeout: float = 5.0,
) -> HB.Heartbeat_1_0 | None:
    """
    Subscribe to Heartbeat, drain until we receive one from *source_node_id*,
    or time out.  Returns the heartbeat message or None on timeout.
    """
    sub = test_node.make_subscriber(HB)
    deadline = asyncio.get_event_loop().time() + timeout
    try:
        while True:
            remaining = deadline - asyncio.get_event_loop().time()
            if remaining <= 0:
                return None
            result = await sub.receive_for(min(remaining, 1.5))
            if result is None:
                continue
            msg, transfer = result
            if transfer.source_node_id == source_node_id:
                return msg  # type: ignore[return-value]
    finally:
        sub.close()
