import { Command } from '@tauri-apps/plugin-shell';
import { platform } from '@tauri-apps/plugin-os';

/**
 * 打开蓝牙设置面板 (推荐)
 * @description
 * macOS: 打开系统设置蓝牙面板
 * Windows: 打开 ms-settings:bluetooth
 */
export const openBluetoothSettings = async () => {
  const osPlatform = await platform();
  try {
    if (osPlatform === 'windows') {
      await Command.create('powershell', ['start', 'ms-settings:bluetooth']).execute();
    } else if (osPlatform === 'macos') {
      await Command.create('open', ['/System/Library/PreferencePanes/Bluetooth.prefPane']).execute();
    }
  } catch (error) {
    console.error('Open Bluetooth settings failed:', error);
    throw error;
  }
};
