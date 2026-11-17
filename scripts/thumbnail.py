"""Capture a batch of Niri windows through one temporary PipeWire session."""
import json
import os
import socket
import struct
import sys
import time

import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstApp", "1.0")
from gi.repository import Gio, GLib, Gst

DEST = "org.gnome.Mutter.ScreenCast"


def crop_scale(width, height, content_width, content_height):
    extra_width, extra_height = width - content_width, height - content_height
    # Only infer modest, symmetric shadow extents; never crop by pixel color.
    if (0 <= extra_width <= content_width / 4 and
            0 <= extra_height <= content_height / 4 and
            abs(extra_width - extra_height) <= 8):
        return width / content_width, height / content_height
    return 1.0, 1.0


def call(path, interface, method, signature="()", args=()):
    return bus.call_sync(DEST, path, interface, method, GLib.Variant(signature, args),
                         None, Gio.DBusCallFlags.NONE, 1500, None).unpack()


def write(window_id, data=b"", width=480, height=300):
    sys.stdout.buffer.write(struct.pack("<qIII", window_id, width, height, len(data)))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def capture(ids, display_scale):
    if not 1 <= display_scale <= 4:
        raise ValueError("invalid display scale")
    max_width, max_height = 560 * display_scale, 240 * display_scale
    session = subscription = None
    pipelines = {}
    pending = set(ids)
    streams = {}
    with socket.socket(socket.AF_UNIX) as ipc:
        ipc.settimeout(1)
        ipc.connect(os.environ["NIRI_SOCKET"])
        ipc.sendall(b'"Windows"\n')
        windows = json.loads(ipc.makefile("rb").readline())["Ok"]["Windows"]
    sizes = {}
    content_sizes = {}
    for window in windows:
        width, height = (size * display_scale for size in window["layout"]["window_size"])
        content_sizes[window["id"]] = (width, height)
        scale = min(max_width / max(1, width), max_height / max(1, height), 1)
        sizes[window["id"]] = (max(1, int(width * scale)), max(1, int(height * scale)))
    loop = GLib.MainLoop()
    try:
        session, = call("/org/gnome/Mutter/ScreenCast", DEST, "CreateSession", "(a{sv})", ({},))
        for window_id in ids:
            stream, = call(session, DEST + ".Session", "RecordWindow", "(a{sv})",
                           ({"window-id": GLib.Variant("t", window_id), "cursor-mode": GLib.Variant("u", 0)},))
            streams[stream] = window_id

        def node_added(_bus, _sender, path, _interface, _signal, parameters):
            window_id = streams.get(path)
            if window_id is None or window_id in pipelines:
                return
            node = parameters.unpack()[0]
            width, height = sizes.get(window_id, (max_width, max_height))
            pipeline = Gst.parse_launch(
                f"pipewiresrc name=source path={node} num-buffers=1 ! glupload ! "
                "gltransformation name=content ortho=true ! glcolorscale ! "
                f"video/x-raw(memory:GLMemory),format=RGBA,width={width},height={height} ! gldownload ! "
                "video/x-raw,format=RGBA ! appsink name=frame sync=false")
            pipelines[window_id] = pipeline
            def crop_source(_pad, info):
                event = info.get_event()
                expected = content_sizes.get(window_id)
                if expected and event.type == Gst.EventType.CAPS:
                    source = event.parse_caps().get_structure(0)
                    x, y = crop_scale(source.get_value("width"), source.get_value("height"), *expected)
                    transform = pipeline.get_by_name("content")
                    transform.set_property("scale-x", x)
                    transform.set_property("scale-y", y)
                return Gst.PadProbeReturn.OK
            # PipeWire includes compositor shadow margins that IPC window sizes exclude.
            pipeline.get_by_name("source").get_static_pad("src").add_probe(Gst.PadProbeType.EVENT_DOWNSTREAM, crop_source)
            pipeline.set_state(Gst.State.PLAYING)

        subscription = bus.signal_subscribe(DEST, DEST + ".Stream", "PipeWireStreamAdded",
                                            None, None, Gio.DBusSignalFlags.NONE, node_added)
        call(session, DEST + ".Session", "Start")
        deadline = time.monotonic() + 2.5

        def collect():
            for window_id, pipeline in list(pipelines.items()):
                sample = pipeline.get_by_name("frame").emit("try-pull-sample", 0)
                if sample is None:
                    continue
                buffer = sample.get_buffer()
                data = buffer.extract_dup(0, buffer.get_size())
                pipeline.set_state(Gst.State.NULL)
                del pipelines[window_id]
                pending.discard(window_id)
                width, height = sizes.get(window_id, (max_width, max_height))
                write(window_id, data if len(data) == width * height * 4 else b"", width, height)
            if not pending or time.monotonic() >= deadline:
                loop.quit()
                return False
            return True

        GLib.timeout_add(20, collect)
        loop.run()
    finally:
        for pipeline in pipelines.values():
            pipeline.set_state(Gst.State.NULL)
        if subscription is not None:
            bus.signal_unsubscribe(subscription)
        if session is not None:
            try:
                call(session, DEST + ".Session", "Stop")
            except GLib.Error:
                pass  # A closed window can already have removed its capture session.
    for window_id in pending:
        write(window_id)


if __name__ == "__main__":
    Gst.init(None)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    capture([int(argument) for argument in sys.argv[2:]], int(sys.argv[1]))
