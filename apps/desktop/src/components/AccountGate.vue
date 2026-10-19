<template>
  <div class="access-screen">
  <header v-if="!isMacOS" class="gate-windowbar" data-tauri-drag-region><span data-tauri-drag-region /><button type="button" aria-label="最小化" @click="win.minimize()">─</button><button type="button" aria-label="最大化" @click="win.toggleMaximize()">□</button><button type="button" aria-label="关闭" @click="win.close()">×</button></header>
  <main class="account-gate">
    <aside class="gate-intro">
      <div class="gate-brand"><BrandMark /><strong>蒲公英</strong></div>
      <div class="gate-copy"><h1>登录你的工作空间</h1><p>登录后，继续对话、查阅知识与处理项目。</p><p class="gate-local">项目和资料保存在这台电脑。账号服务用于验证使用授权与管理设备。</p></div>
      <span class="gate-footer">蒲公英桌面应用</span>
    </aside>
    <section class="gate-content" aria-label="登录与授权验证">
      <AccountPanel :key="identityKey" gate />
    </section>
  </main>
  </div>
</template>
<script setup lang="ts">
import { computed } from 'vue';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isMacOS } from '../lib/platform';
const win=getCurrentWindow();
import AccountPanel from './AccountPanel.vue';
import BrandMark from './BrandMark.vue';
import { accountStatusSnapshot } from '../lib/account-state';
// A revoked session must not leave the previous user's management form mounted.
const identityKey=computed(()=>accountStatusSnapshot.value?.profile?.user.id || 'signed-out');
</script>
<style scoped>
.access-screen{display:flex;flex-direction:column;height:100%}.gate-windowbar{display:flex;height:32px;flex-shrink:0;background:var(--bg-2);border-bottom:1px solid var(--border)}.gate-windowbar>span{flex:1}.gate-windowbar button{width:40px;border:0;background:transparent;color:var(--text);cursor:pointer}.gate-windowbar button:hover{background:var(--hover)}.gate-windowbar button:last-child:hover{background:var(--danger);color:white}
.account-gate{flex:1;min-height:0;display:grid;grid-template-columns:minmax(280px,.85fr) minmax(480px,1.15fr);height:100%;background:var(--bg);color:var(--text);overflow:hidden}
.gate-intro{display:flex;flex-direction:column;justify-content:space-between;padding:48px clamp(28px,5vw,80px);background:var(--bg-2);border-right:1px solid var(--border)}
.gate-brand{display:flex;align-items:center;gap:12px;font-size:22px}.gate-brand :deep(svg){width:34px;height:34px;color:var(--accent)}
.gate-copy{max-width:400px;margin:70px 0}.gate-copy h1{font-size:32px;line-height:1.4;margin:0 0 22px;letter-spacing:-.5px}.gate-copy p{font-size:15px;line-height:1.9;color:var(--muted)}.gate-copy .gate-local{font-size:13px;margin-top:36px}.gate-footer{font-size:12px;color:var(--muted)}
.gate-content{min-width:0;overflow:auto;padding:48px clamp(26px,5vw,80px);overscroll-behavior:contain}.gate-content :deep(.account-panel){max-width:680px;margin:auto}
@media(max-width:850px){.account-gate{grid-template-columns:1fr}.gate-intro{padding:18px 26px;border-right:0;border-bottom:1px solid var(--border)}.gate-copy,.gate-footer{display:none}.gate-content{padding:28px}.account-gate{grid-template-rows:auto minmax(0,1fr)}}
</style>
