import type { Lang } from './api';

const navigation = {
  dashboard: { zh: '系统概览', en: 'Dashboard' },
  cleanup: { zh: '空间回收', en: 'Space Recycle' },
  diskmap: { zh: '空间分析', en: 'Space Analyzer' },
  drivers: { zh: '驱动管理', en: 'Driver Manager' },
  features: { zh: '可选功能', en: 'Optional Features' },
  optimize: { zh: '系统优化', en: 'Optimization' },
  startup: { zh: '开机启动项', en: 'Startup' },
  appx: { zh: '应用卸载管理', en: 'App Uninstaller' },
  rescue: { zh: '系统急救箱', en: 'Rescue Center' },
  snapshots: { zh: '快照回滚', en: 'Snapshots' },
} as const;

export function navigationLabel(tab: keyof typeof navigation, lang: Lang) {
  return navigation[tab][lang];
}
