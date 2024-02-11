import json
import os
import sys

from appdirs import user_data_dir
from ntdrt.log import LoggingSetup

if getattr(sys, "frozen", False) and hasattr(sys, '_MEIPASS'):
    project_dir = sys._MEIPASS
else:
    project_dir = os.path.dirname(__file__)
project_dir = os.path.realpath(project_dir)

data_dir = user_data_dir("mc3000ble", False)
if not os.path.exists(data_dir):
    os.makedirs(data_dir)

log_file = os.path.join(data_dir, "log", "info.log")


def setup_logging(app_name):
    logging_setup = LoggingSetup(app_name)
    logging_setup.file_log = log_file
    logging_setup.setup()


class Config:
    data = {}

    def __init__(self, name="config.json"):
        self.config_file = os.path.join(data_dir, name)

        if os.path.exists(self.config_file):
            with open(self.config_file, "r") as file:
                try:
                    self.data = json.load(file)
                except ValueError:
                    pass

    def list_keys(self):
        return self.data.keys()

    def read(self, name, fallback=None):
        if name in self.data:
            return self.data[name]
        return fallback

    def write(self, name, value, flush=True):
        self.data[name] = value
        if flush:
            self.flush()

    def delete(self, name, flush=True):
        if name in self.data:
            del self.data[name]
        if flush:
            self.flush()

    def flush(self):
        with open(self.config_file, "w") as file:
            json.dump(self.data, file, indent=True)

    def purge(self):
        self.data = {}
