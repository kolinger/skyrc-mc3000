# -*- mode: python -*-

from PyInstaller.utils.hooks import collect_dynamic_libs

debug = False

a = Analysis(['gui.py'],
             binaries=collect_dynamic_libs('bleak'),
             datas=[
                ('assets', 'assets'),
             ],
             hiddenimports=['engineio.async_drivers.threading', 'pytzdata'],
             #hookspath=['pyinstaller'],
             runtime_hooks=[],
             excludes=[],
             win_no_prefer_redirects=False,
             win_private_assemblies=False,
             cipher=None,
             noarchive=False)

pyz = PYZ(a.pure, a.zipped_data,
             cipher=None)

exe = EXE(pyz,
          a.scripts,
          [],
          exclude_binaries=True,
          name='mc3000ble',
          icon='assets/img/icon.ico',
          debug=debug,
          bootloader_ignore_signals=False,
          strip=False,
          upx=False,
          console=False)

coll = COLLECT(exe,
               a.binaries,
               a.zipfiles,
               a.datas,
               strip=False,
               upx=False,
               upx_exclude=[],
               name='mc3000ble')
