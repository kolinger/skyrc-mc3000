import logging
import re
import uuid

from flask import render_template, flash, redirect, url_for, request, Flask, jsonify
import unicodedata
import webview
from werkzeug.exceptions import NotFound

from config import Config
from mc3000usb import MC3000Usb, MC3000Encoder, DeviceNotFoundException, SlotSettings


class ProfilesController:
    def __init__(self, server, config):
        self.server = server
        self.config = config
        self.storage = Config(name="profiles.json")
        self.memory = {}

    def register(self, app: Flask):
        app.add_url_rule("/", "profiles_list", self.handle_list, methods=["GET", "POST"])
        app.add_url_rule("/load", "profiles_load", self.handle_load, methods=["GET", "POST"])
        app.add_url_rule("/import", "profiles_import", self.handle_import, methods=["GET", "POST"])
        app.add_url_rule("/export/<id>", "profiles_export", self.handle_export)
        app.add_url_rule("/set/<id>/<slot>", "profiles_set", self.handle_set)
        app.add_url_rule("/delete/<id>", "profiles_delete", self.handle_delete)

    def before_render(self):
        profiles = []
        for key in self.storage.list_keys():
            profile = SlotSettings()
            profile.fill_fields(self.storage.read(key))
            profiles.append(profile)
        self.server.variables["profiles"] = profiles
        return profiles

    def handle_list(self):
        profiles = self.before_render()
        if request.method == "POST":
            profile: SlotSettings
            keys = list(request.form.keys())
            queue = []
            for profile in profiles:
                name_key = "name-id-%s" % profile.id
                if name_key in request.form:
                    profile.name = request.form[name_key]

                position = keys.index(name_key)
                queue.append((position, profile))

            queue.sort(key=lambda item: item[0])

            self.storage.purge()
            for position, profile in queue:
                self.storage.write(profile.id, profile.get_fields(), flush=False)
            self.storage.flush()

            flash("Changes were successfully saved", category="success")
            return redirect(url_for("profiles_list"))

        return render_template("profiles_list.html")

    def handle_load(self):
        if request.method == "POST":
            if "cancel" in request.form:
                return redirect(url_for("profiles_list"))

            profile: SlotSettings
            for profile in self.memory["current"]:
                if "save-id-%s" % profile.id not in request.form:
                    continue

                name_key = "name-id-%s" % profile.id
                if name_key in request.form:
                    profile.name = request.form[name_key]

                profile.id = self.generate_id()
                self.storage.write(profile.id, profile.get_fields())

                message = "Profile '%s' was successfully saved as '%s'" % (
                    profile.get_description(),
                    profile.name if profile.name else profile.get_description(),
                )
                flash(message, category="success")

            return redirect(url_for("profiles_list"))

        try:
            coms = MC3000Usb()
            encoder = MC3000Encoder()
            profiles = []
            for slot in range(0, 4):
                coms.write(encoder.prepare_slot_settings_read(slot))
                data = coms.read()
                profile = encoder.decode_slot_settings(data)
                profile.id = self.generate_id()
                profiles.append(profile)

            profiles.sort(key=lambda item: item.slot_number)

            self.memory["current"] = profiles

            self.before_render()
            return render_template("profiles_list.html", found=profiles)

        except Exception as e:
            message = self.handle_usb_exception(e)
            flash(message, category="danger")

        return redirect(url_for("profiles_list"))

    def handle_import(self):
        if request.method == "POST":
            if "cancel" in request.form:
                return redirect(url_for("profiles_list"))

            profile: SlotSettings
            for profile in self.memory["current"]:
                if "save-id-%s" % profile.id not in request.form:
                    continue

                name_key = "name-id-%s" % profile.id
                if name_key in request.form:
                    profile.name = request.form[name_key]

                profile.id = self.generate_id()
                self.storage.write(profile.id, profile.get_fields())

                message = "Profile '%s' was successfully saved as '%s'" % (
                    profile.get_description(),
                    profile.name if profile.name else profile.get_description(),
                )
                flash(message, category="success")

            return redirect(url_for("profiles_list"))

        paths = self.server.window.create_file_dialog(
            webview.OPEN_DIALOG, directory="", allow_multiple=True,
            file_types=('JSON files (*.json)', 'All files (*.*)')
        )
        if paths is None:
            return redirect(url_for("profiles_list"))

        profiles = []
        for path in paths:
            try:
                with open(path, "r") as file:
                    profile = SlotSettings()
                    profile.from_json(file.read())
                    profile.id = self.generate_id()
                    profiles.append(profile)
            except Exception as e:
                message = self.handle_usb_exception(e)
                flash(message, category="danger")

        self.memory["current"] = profiles

        self.before_render()
        return render_template("profiles_list.html", found=profiles, imported=True)

    def handle_export(self, id: str):
        profile = self.find_profile(id)

        file_name = "mc3000-%s.json" % self.slugify(profile.get_description(True))
        result = self.server.window.create_file_dialog(
            webview.SAVE_DIALOG, directory="", save_filename=file_name
        )

        if result:
            with open(result, "w") as file:
                file.write(profile.to_json())

        return redirect(url_for("profiles_list"))

    def handle_set(self, id: str, slot: str):
        try:
            profile = self.find_profile(id)

            try:
                selected_slot = int(slot)
            except (TypeError, ValueError):
                selected_slot = None

            coms = MC3000Usb()
            encoder = MC3000Encoder()
            for slot in range(0, 4):
                if selected_slot is not None and selected_slot != slot:
                    continue
                profile.slot_number = slot
                coms.write(encoder.prepare_slot_settings_write(profile))

            if selected_slot is None:
                message = "Profile '%s' was successfully set to all slots" % profile.get_description(True)
            else:
                message = "Profile '%s' was successfully set to slot %s" % (
                    profile.get_description(True), selected_slot + 1
                )

            response = {
                "success": message
            }
        except Exception as e:
            response = {
                "error": self.handle_usb_exception(e),
            }
        return jsonify(response)

    def handle_delete(self, id: str):
        profile = self.find_profile(id)
        self.storage.delete(profile.id)
        flash("Profile '%s' was successfully deleted" % profile.get_description(True), category="success")
        return redirect(url_for("profiles_list"))

    def find_profile(self, id):
        profile = self.storage.read(id)
        if not profile:
            raise NotFound()
        settings = SlotSettings()
        settings.fill_fields(profile)
        return settings

    def slugify(self, value):
        value = str(value)
        value = unicodedata.normalize("NFKD", value).encode("ascii", "ignore").decode("ascii")
        value = re.sub(r"[^\w\s-]", "", value.lower())
        return re.sub(r"[-\s]+", "-", value).strip("-_")

    def handle_usb_exception(self, e):
        suffix = "make sure you charger is connected via USB and driver is working"
        if isinstance(e, DeviceNotFoundException):
            return "Charger not found, %s" % suffix
        else:
            logging.exception(e)
            return "Unexpected error occurred: %s, %s" % (e, suffix)

    def generate_id(self):
        return str(uuid.uuid4())
