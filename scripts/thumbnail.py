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

Gst.init(None)
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
DEST = "org.gnome.Mutter.ScreenCast"


def call(path, interface, method, signature="()", args=()):
    return bus.call_sync(DEST, path, interface, method, GLib.Variant(signature, args),
                         None, Gio.DBusCallFlags.NONE, 1500, None).unpack()


def write(window_id, data=b"", width=480, height=300):
    sys.stdout.buffer.write(struct.pack("<qIII", window_id, width, height, len(data)))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def capture(ids):
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
    for window in windows:
        width, height = window["layout"]["window_size"]
        scale = min(480 / max(1, width), 300 / max(1, height), 1)
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
            width, height = sizes.get(window_id, (480, 300))
            pipeline = Gst.parse_launch(
                f"pipewiresrc path={node} num-buffers=1 ! glupload ! glcolorscale ! "
                f"video/x-raw(memory:GLMemory),format=RGBA,width={width},height={height} ! gldownload ! "
                "video/x-raw,format=RGBA ! appsink name=frame sync=false")
            pipelines[window_id] = pipeline
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
                width, height = sizes.get(window_id, (480, 300))
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
    capture([int(argument) for argument in sys.argv[1:]])
