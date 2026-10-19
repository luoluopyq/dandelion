<template>
  <div class="account-panel">
    <p v-if="error" class="account-alert" role="alert">{{ error }}</p>
    <p v-if="notice" class="account-notice" role="status">{{ notice }}</p>
    <section class="account-section">
      <div class="account-section-title"><div><h2>{{ status?.profile ? '我的账号' : mode === 'login' ? '登录蒲公英' : '注册账号' }}</h2><p>登录后验证账号、有效期和当前设备，授权通过后才能使用应用。</p></div><span class="account-badge">{{ status?.access?.allowed ? planName : status?.profile ? '待授权' : '未登录' }}</span></div>
      <template v-if="status?.profile">
        <div class="account-identity"><span class="account-avatar">{{ (status.profile.user.name || status.profile.user.email).slice(0,1).toUpperCase() }}</span><div><strong>{{ status.profile.user.name || status.profile.user.email }}</strong><p>{{ status.profile.user.email }}</p></div><button type="button" :disabled="busy" @click="logout">退出登录</button></div>
        <dl class="account-facts"><div><dt>当前套餐</dt><dd>{{ planName }}</dd></div><div><dt>有效期</dt><dd>{{ status.profile.user.validUntil ? formatDate(status.profile.user.validUntil) : '长期' }}</dd></div><div><dt>设备</dt><dd>{{ status.profile.devices.length }} / {{ status.profile.maxDevices }}</dd></div></dl>
        <p v-if="!status.access?.allowed" class="account-alert" role="status">{{ status.access?.message || status.message || '当前账号未获得使用授权。' }}</p>
        <h3>已登录设备</h3><div v-for="device in status.profile.devices" :key="device.id" class="account-device"><div><strong>{{ device.name || '未命名设备' }}</strong><p>最近登录 {{ formatDate(device.lastSeen) }}</p></div><button type="button" :disabled="busy" @click="unbind(device.id)">解绑</button></div>
        <button type="button" :disabled="busy" @click="refresh">刷新账号与权益</button>
      </template>
      <form v-else class="account-form" @submit.prevent="submit">
        <label v-if="mode === 'register'">昵称<input v-model="name" autocomplete="nickname" maxlength="80" required /></label>
        <label>邮箱<input v-model="email" type="email" autocomplete="username" maxlength="254" required /></label>
        <label>密码<input v-model="password" type="password" :autocomplete="mode === 'login' ? 'current-password' : 'new-password'" :minlength="mode === 'register' ? 6 : undefined" maxlength="72" required /></label>
        <label v-if="mode === 'register'">确认密码<input v-model="confirmation" type="password" autocomplete="new-password" required /></label>
        <p v-if="mode === 'register'" class="account-help">请使用至少 6 个字符的密码，最多 72 字节。</p>
        <div class="account-actions"><button class="account-primary" :disabled="busy || !status?.configured">{{ busy ? '请稍候…' : mode === 'login' ? '登录' : '注册' }}</button><button type="button" :disabled="busy" @click="switchMode">{{ mode === 'login' ? '创建账号' : '返回登录' }}</button></div>
        <p v-if="!status?.configured" class="account-help">账号服务尚未配置，请联系软件管理员。</p>
      </form>
    </section>
    <section v-if="status?.profile" class="account-section">
      <div class="account-section-title"><div><h2>套餐与订单</h2><p>购买入口在支付接入完成后开放；当前可创建订单并联系管理员确认收款。</p></div></div>
      <div v-for="plan in plans.filter(p => p.key !== 'free')" :key="plan.key" class="account-plan"><div><strong>{{ plan.name }}</strong><p>{{ plan.days }} 天 · 最多 {{ plan.maxDevices }} 台设备</p></div><span>¥{{ (plan.priceCents / 100).toFixed(2) }}</span><button type="button" :disabled="busy" @click="createOrder(plan)">创建订单</button></div>
      <p v-if="!plans.some(p => p.key !== 'free')" class="account-help">暂未上架付费套餐。</p>
      <div v-for="order in orders" :key="order.id" class="account-order"><div><strong>{{ order.plan }} · ¥{{ (order.amountCents / 100).toFixed(2) }}</strong><p>订单号 {{ order.id }}</p></div><span class="account-badge">{{ order.status === 'paid' ? '已收款' : '待付款' }}</span></div>
    </section>
    <section v-if="!gate || showUpdates" class="account-section">
      <div class="account-section-title"><div><h2>软件更新</h2><p>当前版本 {{ status?.version || store.info?.version || '—' }} · {{ channel === 'beta' ? '测试通道' : '正式通道' }}</p></div><button type="button" :disabled="busy || !status?.configured" @click="checkUpdates">{{ checking ? '正在检查…' : '检查更新' }}</button></div>
      <template v-if="update"><div class="account-update"><strong>发现新版本 {{ update.version }}</strong><p>{{ update.notes || '此版本未提供更新说明。' }}</p><button type="button" class="account-primary" :disabled="busy || !status?.canInstall || store.running || store.queued" @click="install">{{ installing ? progressLabel : '下载并安装更新' }}</button><p v-if="store.running || store.queued" class="account-help">请先结束当前任务，再安装更新。</p></div></template>
      <p v-if="!gate && status && !status.updateKeyConfigured" class="account-help">更新签名公钥尚未配置，可检查版本，配置公钥后才能安装。</p>
      <p v-else-if="!gate && status && !status.canInstall" class="account-help">开发环境可检查版本，请使用安装版验证升级安装。</p>
    </section>
    <button v-if="gate" type="button" class="gate-update-link" @click="showUpdates=!showUpdates">{{ showUpdates ? '收起软件更新' : '软件更新' }}</button>
    <details v-if="isDevelopment && (!gate || !status?.configured)" class="account-section account-connection" :open="!status?.configured"><summary>服务连接</summary><p class="account-help">配置部署后的运营后台。正式服务使用 HTTPS，本地测试可填写 http://127.0.0.1:8090。</p><form class="account-form" @submit.prevent="saveConnection"><label>账号服务地址<input v-model="serverUrl" type="url" placeholder="https://accounts.example.com" /></label><label v-if="!gate">更新通道<select v-model="channel"><option value="stable">正式版</option><option value="beta">测试版</option></select></label><label v-if="!gate">更新签名公钥<textarea v-model="publicKey" rows="3" placeholder="发布构建使用的 Tauri 更新公钥" /></label><button class="account-primary" :disabled="busy">保存连接配置</button></form></details>
  </div>
</template>
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { useSessionStore } from '../stores/session';
import * as ipc from '../lib/ipc';
import { setAccountIdentity } from '../lib/account-state';
import type { AccountStatus, AccountPlan, AccountOrder, DesktopUpdate } from '../lib/types';
const {gate=false}=defineProps<{gate?:boolean}>();
const showUpdates=ref(false);
const store=useSessionStore();
const isDevelopment=import.meta.env.DEV;
const status=ref<AccountStatus|null>(null),plans=ref<AccountPlan[]>([]),orders=ref<AccountOrder[]>([]),update=ref<DesktopUpdate|null>(null);
const error=ref(''),notice=ref(''),busy=ref(false),checking=ref(false),installing=ref(false);
const mode=ref<'login'|'register'>('login'),email=ref(''),password=ref(''),confirmation=ref(''),name=ref('');
const serverUrl=ref(''),channel=ref('stable'),publicKey=ref('');
const progress=ref({downloaded:0,total:null as number|null});let unlisten:UnlistenFn|undefined;let disposed=false;
const progressLabel=computed(()=>progress.value.total?`正在下载 ${Math.min(100,Math.round(progress.value.downloaded/progress.value.total*100))}%`:'正在下载并验证…');
const planName=computed(()=>plans.value.find(p=>p.key===status.value?.profile?.user.plan)?.name||status.value?.profile?.user.plan||'免费版');
const formatDate=(v:string)=>new Date(v).toLocaleString('zh-CN');
const message=(e:unknown)=>e instanceof Error?e.message:String(e);
async function run(fn:()=>Promise<void>){if(busy.value)return;busy.value=true;error.value='';notice.value='';try{await fn()}catch(e){error.value=message(e)}finally{busy.value=false}}
async function refresh(){status.value=await ipc.accountStatus();setAccountIdentity(status.value);if(status.value.message)error.value=status.value.message;if(status.value.configured){plans.value=await ipc.accountPlans();orders.value=status.value.profile?await ipc.accountOrders():[]}}
function switchMode(){mode.value=mode.value==='login'?'register':'login';password.value='';confirmation.value='';error.value='';notice.value=''}
async function submit(){await run(async()=>{if(mode.value==='register'){if(password.value!==confirmation.value)throw Error('两次输入的密码不一致');await ipc.accountRegister(email.value,name.value,password.value);mode.value='login';notice.value='注册成功，请登录。管理员开通授权后即可使用应用。'}else{const profile=await ipc.accountLogin(email.value,password.value);await refresh();notice.value=profile.deviceWarning||''}password.value='';confirmation.value=''})}
async function logout(){await run(async()=>{await ipc.accountLogout();await refresh()})}
async function unbind(id:number){await run(async()=>{await ipc.accountUnbindDevice(id);await refresh()})}
async function createOrder(plan:AccountPlan){await run(async()=>{const order=await ipc.accountCreateOrder(plan.key);orders.value=await ipc.accountOrders();notice.value=`订单已创建：${order.id}。在线支付尚未开放，请联系管理员确认收款。`})}
async function checkUpdates(){await run(async()=>{checking.value=true;try{update.value=await ipc.accountCheckUpdate();if(!update.value)notice.value='当前已是此通道的最新版本。'}finally{checking.value=false}})}
async function install(){if(!update.value)return;await run(async()=>{installing.value=true;try{await ipc.accountInstallUpdate(update.value!.version);notice.value='更新已安装，正在重新启动。'}finally{installing.value=false}})}
async function saveConnection(){await run(async()=>{await ipc.accountConnection(serverUrl.value.trim());if(status.value?.access?.allowed){await ipc.settingsPatch({'dandelion.updates.channel':channel.value,'dandelion.updates.publicKey':publicKey.value.trim()});store.settings=await ipc.settingsGet();}update.value=null;await refresh();notice.value='连接配置已保存。'})}
onMounted(async()=>{const values=store.settings?.values||{};serverUrl.value=String(values['dandelion.account.serverUrl']||(import.meta.env.DEV?'http://127.0.0.1:8090':''));channel.value=String(values['dandelion.updates.channel']||'stable');publicKey.value=String(values['dandelion.updates.publicKey']||'');try{const off=await listen<{downloaded:number;total:number|null}>('account-update-progress',e=>{progress.value=e.payload});if(disposed){off();return}unlisten=off}catch(e){error.value=message(e)}await run(async()=>{await refresh();if(!serverUrl.value)serverUrl.value=status.value?.serverUrl||''})});
onBeforeUnmount(()=>{disposed=true;unlisten?.()});
</script>
<style scoped>
.account-panel{max-width:850px}.account-panel .gate-update-link{margin-top:24px;padding:0;border:0;background:transparent;color:var(--accent);font-size:13px}.account-section{padding:26px 0;border-top:1px solid var(--border,#dfdce9)}.account-section:first-of-type{border-top:0;padding-top:0}.account-section-title{display:flex;justify-content:space-between;gap:20px;align-items:flex-start;margin-bottom:24px}.account-section h2{font-size:19px;margin:0 0 7px}.account-section p{color:var(--text-muted,#777183);font-size:13px;line-height:1.7;margin:5px 0}.account-panel button{padding:8px 15px;border:1px solid var(--border,#dfdce9);border-radius:7px;cursor:pointer;background:var(--surface,#fff);color:inherit;white-space:nowrap}.account-panel button:disabled{opacity:.5;cursor:default}.account-panel .account-primary{background:var(--accent,#7864cf);border-color:var(--accent,#7864cf);color:white}.account-form{display:grid;gap:16px;max-width:430px}.account-form label{display:grid;gap:7px;font-size:13px}.account-form input,.account-form textarea,.account-form select{font:inherit;border:1px solid var(--border,#dfdce9);border-radius:7px;padding:10px 12px;background:var(--surface,#fff);color:inherit;width:100%}.account-form textarea{resize:vertical}.account-actions{display:flex;gap:10px;margin-top:5px}.account-badge{background:#ebe7fa;color:#6a55b8;border-radius:6px;padding:4px 9px;font-size:12px;white-space:nowrap}.account-identity{display:flex;gap:14px;align-items:center;margin:20px 0}.account-identity>button{margin-left:auto}.account-avatar{width:46px;height:46px;display:grid;place-items:center;background:#e9e3fa;border-radius:50%;font-size:20px;color:#6a55b8}.account-facts{display:flex;gap:60px;padding:20px 0;margin:10px 0}.account-facts dt{font-size:12px;color:var(--text-muted,#777183)}.account-facts dd{margin:8px 0 0;font-size:15px}.account-device,.account-plan,.account-order{display:flex;align-items:center;justify-content:space-between;gap:15px;border-bottom:1px solid var(--border,#dfdce9);padding:15px 0;margin-bottom:10px}.account-plan>div{flex:1}.account-order p{font-size:12px;overflow-wrap:anywhere}.account-section h3{font-size:14px;margin-top:22px}.account-alert{padding:12px 15px;border-radius:7px;background:#fbecef;color:#a43249!important;white-space:pre-wrap}.account-notice{padding:12px 15px;background:#eeebfa;color:#6550a2;border-radius:7px}.account-update{padding:18px;background:var(--surface,#fff);border:1px solid var(--border,#dfdce9);border-radius:8px}.account-update p{white-space:pre-wrap;margin:10px 0 16px}.account-connection summary{cursor:pointer;font-weight:600}.account-connection .account-form{margin-top:20px}.account-panel :focus-visible{outline:3px solid #b8a9fa;outline-offset:2px}@media(max-width:700px){.account-facts{gap:22px;flex-wrap:wrap}.account-section-title{flex-wrap:wrap}.account-plan{flex-wrap:wrap}}
</style>
