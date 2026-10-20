import { ref, computed } from "vue";
import type { AccountStatus } from "./types";

// Only display identity is shared with application chrome. Credentials stay in Rust.
export const accountStatusSnapshot = ref<AccountStatus|null>(null);
// grace：后端在短暂断网/超时时的有界宽限标记。授权未被撤销就保持界面可用；
// 明确撤销路径下后端恒返回 grace=false，限制不会被放宽。
export const applicationAccessGranted = computed(
  () => !!accountStatusSnapshot.value?.access?.allowed || accountStatusSnapshot.value?.grace === true,
);
export const accountDisplayName = ref("");

export function setAccountIdentity(status: AccountStatus) {
  accountStatusSnapshot.value = status;
  const user = status.profile?.user;
  accountDisplayName.value = user ? user.name || user.email : "";
}
