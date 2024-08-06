MC3000 BLE monitor and USB Profiles
===================================

Simple application to view status of SkyRC MC3000 over Bluetooth on desktop computers 
with additional application to manage profiles over USB.

This project provides two different applications:
1) **BLE** monitor - this application shows current state of the charger
   and provides notifications over Bluetooth (BLE).
2) **USB Profiles** - this application can manage profiles over USB.

This project was originally just the **BLE** monitor that's why the structure may be a bit confusing.
Later the **MC3000 USB Profiles** application was added - this application allows you to export/import/share profiles
from your MC3000 over USB cable. This application operates independently of BLE monitor application. 
This separation exists mainly due to the hardware limitation of the MC3000 where different communication interfaces
don't provide all the functions.

Both the **BLE** monitor and **USB Profiles** share the same executable `mc3000ble.exe`.
The executable decided what application to launch by first argument - the value `profiles` 
means **USB Profiles** anything else means **BLE** monitor. 
The installation creates shortcuts for both the **MC3000 BLE** monitor and **MC3000 USB Profiles**.

Written in Python, cross-platform in theory with some tweaks.\
Windows x64 installer provided in [releases](https://github.com/kolinger/skyrc-mc3000/releases).

**BLE monitor application:**

![monitor](resources/monitor.png)


**USB Profiles application:**

![profiles](resources/profiles.png)

Development
-----------

- Requirements
  - Python 3.7
  - `pip install -r requirements.txt`
- Building
  - `pyinstaller --noconfirm pyinstaller.spec`
  - `makensis.exe installer.nsi`
