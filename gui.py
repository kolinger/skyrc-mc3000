import asyncio
from http.client import HTTPConnection
import json
import logging
import os
import socket
import sys
from threading import Thread
from time import sleep, time
from urllib.parse import quote_plus

import bleak
import flask
from flask import render_template, Flask, jsonify, request, redirect, url_for
from notifypy import Notify
import pendulum
import webview

import config
from config import project_dir
from mc3000ble import MC3000Ble


class Server:
    address = None
    separator = True
    service_thread = None
    service = None
    timeout_thread = None
    last_update_time = None
    waiting = True
    scan_thread = None
    loop = None
    previous = {}

    def __init__(self, title):
        self.title = title
        self.config = config.Config()

        gui_dir = os.path.join(os.path.dirname(__file__), "..", "..", "assets")
        if not os.path.exists(gui_dir):
            gui_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "assets")

        self.app = app = Flask(__name__, static_folder=gui_dir, template_folder=gui_dir)
        app.config["SEND_FILE_MAX_AGE_DEFAULT"] = 1

        app.context_processor(self.context_processor)
        app.after_request(self.after_request)

        app.add_url_rule("/", "index", self.index)
        app.add_url_rule("/scan", "scan", self.scan)
        app.add_url_rule("/scan/trigger", "scan_trigger", self.scan_trigger)
        app.add_url_rule("/scan/select", "scan_select", self.scan_select)

        self.notify = Notify(
            default_notification_title="SkyRC MC3000 status update",
            default_notification_application_name="SkyRC MC3000 BLE",
            default_notification_icon=os.path.join(project_dir, "assets", "img", "icon.ico"),
        )

    def context_processor(self):
        return {
            "url_for": self.url_for,
        }

    def after_request(self, response):
        response.headers["Cache-Control"] = "no-store"
        return response

    def index(self):
        if self.config.read("ble_address") is None:
            return redirect(url_for("scan"))
        return render_template("index.html")

    def scan(self):
        return render_template("scan.html")

    def scan_trigger(self):
        if self.scan_thread is None:
            self.scan_thread = Thread(target=self._background_scan, daemon=True)
            self.scan_thread.start()
        return jsonify({
            "status": "ok",
        })

    def scan_select(self):
        ble_address = request.args.get("ble_address")
        self.config.write("ble_address", ble_address)
        self.spawn_service()
        return jsonify({
            "status": "ok",
        })

    def _background_scan(self):
        if self.service_thread is not None:
            self.service.stop()
            self.service_thread.join()
            self.service_thread = None

        async def run():
            scanner = bleak.BleakScanner()
            devices = await scanner.discover()
            formatted = []
            for device in devices:
                formatted.append({
                    "address": device.address,
                    "name": device.name,
                })
            return formatted

        data = self.get_loop().run_until_complete(run())

        results = ["Results:"]
        if len(data) == 0:
            results.append("no device found, try again")

        for device in data:
            name = device["address"] + " (" + device["name"] + ")"
            results.append("<a href=\"#\" data-address=\"" + device["address"] + "\">" + name + "</a>")

        self.update_client_side({
            "scan_results": "<br>".join(results),
        })

        self.scan_thread = None

    def update_client_side(self, payload):
        for window in webview.windows:
            payload = json.dumps(payload)
            payload = payload.replace("\\", "\\\\")
            window.evaluate_js("window.app.update('" + payload + "');")

    def set_title(self, title):
        for window in webview.windows:
            window.set_title(title)

    def url_for(self, endpoint, **values):
        if endpoint == "static":
            filename = values.get("filename", None)
            if filename:
                file_path = os.path.dirname(os.path.realpath(__file__)) + "/assets/" + filename
                if os.path.exists(file_path):
                    values["v"] = int(os.stat(file_path).st_mtime)
        return flask.url_for(endpoint, **values)

    def get_loop(self):
        if not self.loop:
            self.loop = asyncio.new_event_loop()
        return self.loop

    def run(self, host="127.0.0.1"):
        logging.getLogger("werkzeug").setLevel(logging.WARNING)

        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.bind((host, 0))
        port = sock.getsockname()[1]
        sock.close()

        self.address = (host, port)

        self.spawn_service()
        self.spawn_timeout()
        self.app.run(host=host, port=port, threaded=True, use_reloader=False)

    def spawn_service(self):
        if self.config.read("ble_address") is None:
            return

        if self.service_thread is None:
            self.service_thread = Thread(target=self._background_service, daemon=True)
            self.service_thread.start()

    def spawn_timeout(self):
        if self.timeout_thread is None:
            self.timeout_thread = Thread(target=self._background_timeout, daemon=True)
            self.timeout_thread.start()

    def _background_service(self):
        self.service = MC3000Ble(ble_address=self.config.read("ble_address"))
        while True:
            try:
                self.service.run(self._update)
            except (bleak.BleakError, asyncio.TimeoutError) as e:
                logging.exception(e)
                if self.service.running:
                    sleep(1)

            if not self.service.running:
                break

    def _update(self, battery_info):
        if battery_info["time"].total_seconds() == 0:
            battery_info["time"] = "0 seconds"
        else:
            time_string = battery_info["time"].in_words(separator=";")
            time_pieces = time_string.split(";")
            while len(time_pieces) > 2:
                time_pieces.pop(-1)
            battery_info["time"] = " ".join(time_pieces)

        self.update_client_side({
            "battery_info": battery_info,
        })

        self.last_update_time = time()

        slot_index = battery_info["slot"]
        if slot_index == 3:
            separator = "•" if self.separator else "⁃"
            self.separator = not self.separator
            self.set_title("%s %s %s" % (self.title, separator, pendulum.now().format("HH:mm:ss")))

        if slot_index not in self.previous:
            self.previous[slot_index] = None

        if battery_info["status"] not in ["standby", "charge", "discharge"]:
            if self.previous[slot_index] != battery_info["status"]:
                self.notify.message = "Slot %s changed status to '%s'" % (slot_index + 1, battery_info["status"])
                self.notify.send()

        if self.previous[slot_index] != battery_info["status"]:
            self.previous[slot_index] = battery_info["status"]

    def _background_timeout(self):
        while True:
            if self.last_update_time is not None:
                if self.last_update_time < time() - 10:
                    if not self.waiting:
                        self.set_title("%s - %s" % (self.title, "waiting for connection"))
                        self.notify.message = "Lost connection with charger!"
                        self.notify.send()
                    self.waiting = True
                else:
                    self.waiting = False

            sleep(1)


class Api:
    def __init__(self, url, port):
        self.url = url
        self.port = port

    def is_up(self):
        try:
            connection = HTTPConnection(self.url, self.port, timeout=0.100)
            connection.request("GET", "/")
            response = connection.getresponse()
            return response.status in [200, 302]
        except:
            return False


if __name__ == "__main__":
    config.setup_logging("MC3000")

    try:
        server = Server("MC3000")
        Thread(target=server.run, daemon=True).start()

        while not isinstance(server.address, tuple):
            sleep(0.010)

        api = Api(*server.address)
        url = "http://%s:%s" % server.address
        name = "loading.html?url=%s" % quote_plus(url)
        url = os.path.join(config.project_dir, "assets", name)
        title = "%s - %s" % (server.title, "waiting for connection")
        webview.create_window(title, url, js_api=api, width=780, height=340, text_select=True)
        webview.start(debug="FLASK_DEBUG" in os.environ, gui="cef")
    except SystemExit:
        raise
    except KeyboardInterrupt:
        exit(1)
    except:
        logging.exception(sys.exc_info()[0])
