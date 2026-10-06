// Punto de entrada del bundle nativo de Android.
// esbuild empaqueta esto (más @capacitor/core y los plugins) hacia
// OWS-Desktop/app/vendor/cap-native.js. app.js solo inyecta ese <script>
// cuando Capacitor.getPlatform() === 'android'; en web y en el Hub
// desktop (Tauri) el bundle jamás se descarga.
import { Capacitor } from '@capacitor/core';
import { App } from '@capacitor/app';
import { Filesystem, Directory, Encoding } from '@capacitor/filesystem';
import { FileTransfer } from '@capacitor/file-transfer';
import { FileOpener } from '@capacitor-community/file-opener';
import { SplashScreen } from '@capacitor/splash-screen';
import { StatusBar, Style } from '@capacitor/status-bar';

window.OWSNative = {
  platform: Capacitor.getPlatform(),
  isNative: Capacitor.isNativePlatform(),
  App,
  Filesystem,
  Directory,
  Encoding,
  FileTransfer,
  FileOpener,
  SplashScreen,
  StatusBar,
  Style
};
