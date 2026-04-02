"""
Destructive OTA integration test using uavcan.file.Read.

This test is opt-in and only runs with --run-ota because it stages firmware
and reboots the device.
"""
from __future__ import annotations

import asyncio
import logging
import pathlib

import pycyphal.application
import pycyphal.presentation
import pytest

import uavcan.file.Error_1_0 as FileError
import uavcan.file.Read_1_1 as Read
import uavcan.node.ExecuteCommand_1_3 as EC
import uavcan.node.GetInfo_1_0 as GI
import uavcan.primitive.Unstructured_1_0 as Unstructured

from .conftest import wait_for_heartbeat, _PnpAllocator


# Per uavcan.file.Read semantics, non-full chunks indicate EOF.
# Therefore all non-terminal responses must use the full transfer capacity.
READ_CHUNK_SIZE = 256
LOGGER = logging.getLogger(__name__)


@pytest.mark.ota
@pytest.mark.destructive
async def test_begin_software_update_downloads_and_reboots(
    run_ota: bool,
    ota_file_path: str,
    ota_image_file: str,
    ec_client: pycyphal.presentation.Client,
    gi_client: pycyphal.presentation.Client,
    test_node: pycyphal.application.Node,
    dut_node_id: int,
    pnp_allocator: _PnpAllocator
) -> None:
    """
    End-to-end OTA flow:
    1. Start local uavcan.file.Read server on the test harness node.
    2. Send COMMAND_BEGIN_SOFTWARE_UPDATE with OTA path.
    3. Verify DUT requests file blocks from this server.
    4. Verify DUT reboots and comes back online.
    5. Verify GetInfo still responds post-reboot.
    """
    if not run_ota:
        pytest.skip("OTA test disabled. Re-run with --run-ota")

    image_path = pathlib.Path(ota_image_file)
    if not image_path.exists():
        pytest.skip(f"OTA image file not found: {image_path}")

    image_data = image_path.read_bytes()
    if not image_data:
        pytest.skip(f"OTA image file is empty: {image_path}")

    read_server = test_node.get_server(Read)
    requests_seen = 0
    wrong_path_requests = 0
    eof_seen = asyncio.Event()
    first_request_seen = asyncio.Event()

    async def handle_read(request: Read.Request, _metadata):
        nonlocal requests_seen, wrong_path_requests
        requests_seen += 1
        first_request_seen.set()

        requested_path = bytes(request.path.path).decode("utf-8", errors="replace")
        offset = int(request.offset)
        source_node = getattr(_metadata, "client_node_id", None)
        LOGGER.warning(
            "OTA server got file.read req#%d source=%s offset=%d path=%r",
            requests_seen,
            source_node,
            offset,
            requested_path,
        )
        if requested_path != ota_file_path:
            wrong_path_requests += 1
            LOGGER.warning(
                "OTA server path mismatch expected=%r got=%r",
                ota_file_path,
                requested_path,
            )
            return Read.Response(
                error=FileError(value=FileError.NOT_FOUND),
                data=Unstructured(value=[]),
            )

        if offset >= len(image_data):
            chunk = b""
        else:
            chunk = image_data[offset : offset + READ_CHUNK_SIZE]

        if len(chunk) < READ_CHUNK_SIZE:
            eof_seen.set()

        return Read.Response(
            error=FileError(value=FileError.OK),
            data=Unstructured(value=list(chunk)),
        )

    read_server.serve_in_background(handle_read)
    try:
        await asyncio.sleep(0)
        begin = EC.Request(
            command=EC.Request.COMMAND_BEGIN_SOFTWARE_UPDATE,
            parameter=list(ota_file_path.encode("utf-8")),
        )
        result = await ec_client.call(begin)
        assert result is not None, "BEGIN_SOFTWARE_UPDATE request timed out"
        response, _ = result
        LOGGER.warning("BEGIN_SOFTWARE_UPDATE response status=%s", response.status)
        LOGGER.warning("Read server stats after begin: %s", read_server.sample_statistics())
        assert response.status == EC.Response.STATUS_SUCCESS, (
            f"Expected STATUS_SUCCESS, got {response.status}"
        )

        await asyncio.wait_for(first_request_seen.wait(), timeout=10.0)
        LOGGER.warning("OTA server observed first file.read request")

        await asyncio.wait_for(eof_seen.wait(), timeout=300.0)
        LOGGER.warning("Read server stats after EOF: %s", read_server.sample_statistics())
        assert requests_seen >= 2, "Expected multiple file.Read requests during OTA"
        assert wrong_path_requests == 0, (
            f"DUT requested unexpected file path {wrong_path_requests} time(s)"
        )
        # Reset The Allocation
        pnp_allocator.reset()
        await pnp_allocator.wait_allocated(timeout=90.0)


        hb = await wait_for_heartbeat(test_node, dut_node_id, timeout=30.0)
        assert hb is not None, "DUT did not come back online after OTA reboot"


        info_result = await gi_client.call(GI.Request())
        assert info_result is not None, "GetInfo timed out after OTA reboot"
        print("GetInfo response after OTA reboot: %s", info_result)
    finally:
        LOGGER.warning("Read server final stats: %s", read_server.sample_statistics())
        read_server.close()
