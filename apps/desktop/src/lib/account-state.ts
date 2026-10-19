import { ref, computed } from "vue";
import type { AccountStatus } from "./types";

// Only display identity is shared with application chrome. Credentials stay in Rust.
export const accountStatusSnapshot = ref<AccountStatus|null>(null);
export const applicationAccessGranted = computed(() => !!accountStatusSnapshot.value?.access?.allowed);
export const accountDisplayName = ref("");

export function setAccountIdentity(status: AccountStatus) {
  accountStatusSnapshot.value = status;
  const user = status.profile?.user;
  accountDisplayName.value = user ? user.name || user.email : "";
}
