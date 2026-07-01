<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { Loader2, LogOut, Plus, RefreshCw, Search } from 'lucide-vue-next'
import Button from '@/components/ui/Button.vue'
import Input from '@/components/ui/Input.vue'
import Label from '@/components/ui/Label.vue'
import Select from '@/components/ui/Select.vue'
import SegmentedControl from '@/components/ui/SegmentedControl.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import Textarea from '@/components/ui/Textarea.vue'
import { useToast } from '@/composables/useToast'
import {
  api,
  type DashboardStats,
  type RuleWithTraffic,
  type UpdateRulePayload,
} from '@/lib/api'
import { formatBytes, hasInputValue, quotaPeriodLabel } from '@/lib/utils'
import { useAuthStore } from '@/stores/auth'

type PortMode = 'auto' | 'specific' | 'manual'
type QuotaPeriod = 'total' | 'daily' | 'monthly'

const router = useRouter()
const auth = useAuthStore()
const toast = useToast()

const rules = ref<RuleWithTraffic[]>([])
const stats = ref<DashboardStats | null>(null)
const loading = ref(false)
const submitting = ref(false)
const search = ref('')

const mode = ref<PortMode>('auto')
const startPort = ref('')
const manualPorts = ref('')
const targets = ref('')
const quotaGb = ref('')
const quotaPeriod = ref<QuotaPeriod>('total')

const selected = ref<Set<number>>(new Set())
const editing = ref<RuleWithTraffic | null>(null)
const editTargetHost = ref('')
const editTargetPort = ref('')
const editQuotaGb = ref('')
const editQuotaPeriod = ref<QuotaPeriod>('total')
const editUnsetQuota = ref(false)
const editSaving = ref(false)

const quotaHasValue = computed(() => hasInputValue(quotaGb.value))
const editQuotaHasValue = computed(() => hasInputValue(editQuotaGb.value))

const modeOptions = [
  { value: 'auto', label: '自动' },
  { value: 'specific', label: '起始端口' },
  { value: 'manual', label: '手动' },
]

const quotaPeriodOptions = [
  { value: 'total', label: '累计' },
  { value: 'daily', label: '每日' },
  { value: 'monthly', label: '每月' },
]

const filteredRules = computed(() => {
  const q = search.value.trim().toLowerCase()
  if (!q) return rules.value
  const terms = q.split(',').map((t) => t.trim()).filter(Boolean)
  return rules.value.filter((item) =>
    terms.some(
      (t) =>
        String(item.rule.local_port).includes(t) ||
        item.rule.target_host.toLowerCase().includes(t) ||
        String(item.rule.target_port).includes(t),
    ),
  )
})

const allSelected = computed({
  get: () =>
    filteredRules.value.length > 0 &&
    filteredRules.value.every((r) => selected.value.has(r.rule.local_port)),
  set: (v: boolean) => {
    filteredRules.value.forEach((r) => {
      if (v) selected.value.add(r.rule.local_port)
      else selected.value.delete(r.rule.local_port)
    })
  },
})

function trafficTotal(item: RuleWithTraffic) {
  const t = item.traffic.totals
  return t.tcp_rx + t.tcp_tx + t.udp_rx + t.udp_tx
}

function trafficBreakdown(item: RuleWithTraffic) {
  const t = item.traffic.totals
  return { tcp: t.tcp_rx + t.tcp_tx, udp: t.udp_rx + t.udp_tx }
}

function quotaLabel(item: RuleWithTraffic) {
  const q = item.rule.quota_bytes
  if (!q) return '不限'
  return `${formatBytes(q)} · ${quotaPeriodLabel(item.rule.quota_period)}`
}

function quotaPercent(item: RuleWithTraffic) {
  return Math.round((item.traffic.quota_used_ratio ?? 0) * 100)
}

function isQuotaBlocked(item: RuleWithTraffic) {
  return (
    !item.rule.enabled &&
    item.rule.quota_bytes != null &&
    (item.traffic.quota_used_ratio ?? 0) >= 1
  )
}

function statusVariant(item: RuleWithTraffic): 'success' | 'warning' | 'muted' {
  if (item.rule.enabled) return 'success'
  if (isQuotaBlocked(item)) return 'warning'
  return 'muted'
}

function statusLabel(item: RuleWithTraffic) {
  if (item.rule.enabled) return '运行中'
  if (isQuotaBlocked(item)) return '配额停服'
  return '已停用'
}

function toggleSelect(port: number, checked: boolean) {
  if (checked) selected.value.add(port)
  else selected.value.delete(port)
}

async function refresh() {
  loading.value = true
  try {
    const [list, s] = await Promise.all([api.listRules(), api.stats()])
    rules.value = list
    stats.value = s
    selected.value.clear()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '加载失败')
  } finally {
    loading.value = false
  }
}

async function submitRules() {
  if (!targets.value.trim()) {
    toast.error('请输入目标列表')
    return
  }
  const lines = targets.value.split('\n').filter((l) => l.trim())
  const body: Parameters<typeof api.addRules>[0] = { mode: mode.value, targets: targets.value }

  if (mode.value === 'specific') {
    if (!hasInputValue(startPort.value)) return toast.error('请输入起始端口')
    body.start_port = Number(startPort.value)
  }
  if (mode.value === 'manual') {
    const ports = manualPorts.value.split(',').map((p) => p.trim()).filter(Boolean)
    if (ports.length !== lines.length) {
      return toast.error(`端口数 (${ports.length}) 与目标数 (${lines.length}) 不一致`)
    }
    body.ports = ports.map(Number)
  }
  if (quotaHasValue.value) {
    body.quota_gb = Number(quotaGb.value)
    body.quota_period = quotaPeriod.value
  }

  submitting.value = true
  try {
    const res = await api.addRules(body)
    if (!res.success) return toast.error(res.message ?? '添加失败')
    toast.success(res.message ?? '添加成功')
    targets.value = ''
    startPort.value = ''
    manualPorts.value = ''
    quotaGb.value = ''
    quotaPeriod.value = 'total'
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '添加失败')
  } finally {
    submitting.value = false
  }
}

function openEdit(item: RuleWithTraffic) {
  editing.value = item
  editTargetHost.value = item.rule.target_host
  editTargetPort.value = String(item.rule.target_port)
  editUnsetQuota.value = false
  editQuotaGb.value = item.rule.quota_bytes
    ? String((item.rule.quota_bytes / 1024 ** 3).toFixed(2))
    : ''
  const p = item.rule.quota_period
  editQuotaPeriod.value = p === 'daily' || p === 'monthly' ? p : 'total'
}

function closeEdit() {
  editing.value = null
}

async function saveEdit() {
  if (!editing.value) return
  const port = editing.value.rule.local_port
  const body: UpdateRulePayload = {
    target_host: editTargetHost.value.trim(),
    target_port: Number(editTargetPort.value),
  }
  if (editUnsetQuota.value) {
    body.unset_quota = true
  } else if (editQuotaHasValue.value) {
    body.quota_gb = Number(editQuotaGb.value)
    body.quota_period = editQuotaPeriod.value
  }

  editSaving.value = true
  try {
    const res = await api.updateRule(port, body)
    if (!res.success) return toast.error(res.message ?? '保存失败')
    toast.success('规则已更新')
    closeEdit()
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '保存失败')
  } finally {
    editSaving.value = false
  }
}

async function toggleRule(item: RuleWithTraffic) {
  try {
    const res = await api.toggleRule(item.rule.local_port)
    if (!res.success) return toast.error(res.message ?? '操作失败')
    toast.success(item.rule.enabled ? '已停用' : '已启用')
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '操作失败')
  }
}

async function resetTraffic(item: RuleWithTraffic) {
  if (!confirm(`重置端口 ${item.rule.local_port} 的流量统计？`)) return
  try {
    const res = await api.resetTraffic(item.rule.local_port)
    if (!res.success) return toast.error(res.message ?? '重置失败')
    toast.success('流量已重置')
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '重置失败')
  }
}

async function removeRule(port: number) {
  if (!confirm(`确定删除端口 ${port}？`)) return
  try {
    const res = await api.deleteRule(port)
    if (!res.success) return toast.error(res.message ?? '删除失败')
    toast.success('已删除')
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '删除失败')
  }
}

async function removeSelected() {
  const ports = [...selected.value]
  if (!ports.length) return toast.error('请先选择规则')
  if (!confirm(`删除选中的 ${ports.length} 条规则？`)) return
  try {
    const res = await api.deleteBatch(ports)
    if (res.data?.failed.length) toast.error(`部分失败: ${res.data.failed.join(', ')}`)
    else toast.success('批量删除成功')
    await refresh()
  } catch (e) {
    toast.error(e instanceof Error ? e.message : '删除失败')
  }
}

function logout() {
  auth.logout()
  router.push({ name: 'login' })
}

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  refresh()
  timer = setInterval(refresh, 30_000)
})
onUnmounted(() => clearInterval(timer))
</script>

<template>
  <div class="min-h-dvh bg-background">
    <header class="border-b border-border">
      <div class="page-shell flex min-h-14 flex-wrap items-center justify-between gap-2 py-2">
        <div class="text-sm font-medium tracking-tight shrink-0">Realm 转发</div>
        <div class="flex items-center gap-1 shrink-0">
          <Button variant="ghost" size="sm" :disabled="loading" @click="refresh">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" />
            刷新
          </Button>
          <Button variant="ghost" size="sm" @click="logout">
            <LogOut class="h-3.5 w-3.5" />
            退出
          </Button>
        </div>
      </div>
    </header>

    <main class="page-shell">
      <section class="section !py-5">
        <div class="grid grid-cols-2 gap-4 sm:grid-cols-4 sm:gap-6">
          <div class="stat-item">
            <span class="stat-label">规则总数</span>
            <span class="stat-value">{{ stats?.rule_count ?? '—' }}</span>
          </div>
          <div class="stat-item">
            <span class="stat-label">运行中</span>
            <span class="stat-value">{{ stats?.active_count ?? '—' }}</span>
          </div>
          <div class="stat-item">
            <span class="stat-label">总流量</span>
            <span class="stat-value">{{ stats ? formatBytes(stats.total_traffic_bytes) : '—' }}</span>
          </div>
          <div class="stat-item">
            <span class="stat-label">配额停服</span>
            <span class="stat-value">{{ stats?.quota_blocked_count ?? '—' }}</span>
          </div>
        </div>
      </section>

      <section class="section">
        <h2 class="section-title">添加转发</h2>
        <div class="space-y-4">
          <SegmentedControl v-model="mode" :options="modeOptions" />
          <div class="grid gap-4 sm:grid-cols-2">
            <div v-if="mode === 'specific'" class="space-y-1">
              <Label>起始端口</Label>
              <Input v-model="startPort" type="number" placeholder="8000" />
            </div>
            <div v-if="mode === 'manual'" class="space-y-1 sm:col-span-2">
              <Label>本地端口（逗号分隔）</Label>
              <Input v-model="manualPorts" placeholder="8001,8002" />
            </div>
            <div class="space-y-1">
              <Label>流量配额 (GB，可选)</Label>
              <Input v-model="quotaGb" type="number" step="0.1" placeholder="不限" />
            </div>
            <div v-if="quotaHasValue" class="space-y-1">
              <Label>配额周期</Label>
              <Select v-model="quotaPeriod">
                <option v-for="opt in quotaPeriodOptions" :key="opt.value" :value="opt.value">
                  {{ opt.label }}
                </option>
              </Select>
            </div>
          </div>
          <div class="space-y-1">
            <Label>目标列表（每行 主机:端口）</Label>
            <Textarea v-model="targets" placeholder="192.168.1.10:8080&#10;10.0.0.2:443" />
          </div>
          <Button :disabled="submitting" @click="submitRules">
            <Loader2 v-if="submitting" class="h-3.5 w-3.5 animate-spin" />
            <Plus v-else class="h-3.5 w-3.5" />
            添加
          </Button>
        </div>
      </section>

      <section class="section border-b-0">
        <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between mb-4">
          <h2 class="section-title !mb-0 shrink-0">规则列表 · {{ rules.length }}</h2>
          <div class="flex flex-col gap-2 sm:flex-row sm:items-center w-full sm:w-auto">
            <div class="relative w-full sm:w-56">
              <Search class="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground pointer-events-none" />
              <Input v-model="search" class="pl-9 w-full" placeholder="搜索…" />
            </div>
            <Button variant="destructive" size="sm" class="w-full sm:w-auto shrink-0" :disabled="!selected.size" @click="removeSelected">
              删除 ({{ selected.size }})
            </Button>
          </div>
        </div>

        <div class="hidden md:block overflow-x-auto">
          <table class="w-full text-sm">
            <thead class="border-b border-border text-left text-xs text-muted-foreground">
              <tr>
                <th class="w-10 pb-3"><input v-model="allSelected" type="checkbox" /></th>
                <th class="pb-3 font-medium">端口</th>
                <th class="pb-3 font-medium">目标</th>
                <th class="pb-3 font-medium">状态</th>
                <th class="pb-3 font-medium">流量</th>
                <th class="pb-3 font-medium">配额</th>
                <th class="pb-3 font-medium text-right">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="loading && !rules.length">
                <td colspan="7" class="py-10 text-center text-muted-foreground">
                  <Loader2 class="inline h-4 w-4 animate-spin" />
                </td>
              </tr>
              <tr v-else-if="!filteredRules.length">
                <td colspan="7" class="py-10 text-center text-muted-foreground">暂无规则</td>
              </tr>
              <tr
                v-for="item in filteredRules"
                v-else
                :key="item.rule.id"
                class="border-b border-border/70 hover:bg-muted/30"
              >
                <td class="py-3">
                  <input
                    type="checkbox"
                    :checked="selected.has(item.rule.local_port)"
                    @change="toggleSelect(item.rule.local_port, ($event.target as HTMLInputElement).checked)"
                  />
                </td>
                <td class="py-3 font-mono font-medium tabular-nums">{{ item.rule.local_port }}</td>
                <td class="py-3 font-mono text-xs text-muted-foreground">
                  {{ item.rule.target_host }}:{{ item.rule.target_port }}
                </td>
                <td class="py-3">
                  <StatusBadge :variant="statusVariant(item)">{{ statusLabel(item) }}</StatusBadge>
                </td>
                <td class="py-3">
                  <div class="tabular-nums">{{ formatBytes(trafficTotal(item)) }}</div>
                  <div class="text-[11px] text-muted-foreground mt-0.5">
                    TCP {{ formatBytes(trafficBreakdown(item).tcp) }} · UDP {{ formatBytes(trafficBreakdown(item).udp) }}
                  </div>
                </td>
                <td class="py-3">
                  <div v-if="item.rule.quota_bytes" class="space-y-1">
                    <div class="text-xs text-muted-foreground">{{ quotaLabel(item) }}</div>
                    <div class="h-0.5 w-16 bg-muted overflow-hidden">
                      <div
                        class="h-full"
                        :class="quotaPercent(item) >= 100 ? 'bg-destructive' : 'bg-primary'"
                        :style="{ width: `${Math.min(quotaPercent(item), 100)}%` }"
                      />
                    </div>
                  </div>
                  <span v-else class="text-xs text-muted-foreground">不限</span>
                </td>
                <td class="py-3 text-right">
                  <div class="flex justify-end gap-0.5 flex-wrap">
                    <Button variant="ghost" size="sm" @click="openEdit(item)">编辑</Button>
                    <Button variant="ghost" size="sm" @click="toggleRule(item)">
                      {{ item.rule.enabled ? '停用' : '启用' }}
                    </Button>
                    <Button variant="ghost" size="sm" @click="resetTraffic(item)">清零</Button>
                    <Button variant="ghost" size="sm" class="text-destructive" @click="removeRule(item.rule.local_port)">删除</Button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="md:hidden space-y-3">
          <div v-if="loading && !rules.length" class="py-8 text-center text-muted-foreground text-sm">
            <Loader2 class="inline h-4 w-4 animate-spin" />
          </div>
          <div v-else-if="!filteredRules.length" class="py-8 text-center text-muted-foreground text-sm">暂无规则</div>
          <article
            v-for="item in filteredRules"
            v-else
            :key="item.rule.id"
            class="rounded-lg border border-border bg-card p-4 space-y-3"
          >
            <div class="flex items-start gap-3">
              <input
                type="checkbox"
                class="mt-1 shrink-0"
                :checked="selected.has(item.rule.local_port)"
                @change="toggleSelect(item.rule.local_port, ($event.target as HTMLInputElement).checked)"
              />
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2 flex-wrap">
                  <span class="font-mono font-semibold tabular-nums">:{{ item.rule.local_port }}</span>
                  <StatusBadge :variant="statusVariant(item)">{{ statusLabel(item) }}</StatusBadge>
                </div>
                <div class="mt-1 text-xs font-mono text-muted-foreground break-all">
                  → {{ item.rule.target_host }}:{{ item.rule.target_port }}
                </div>
              </div>
            </div>

            <div class="grid grid-cols-2 gap-3 text-xs pl-7">
              <div>
                <div class="text-muted-foreground mb-0.5">流量</div>
                <div class="tabular-nums font-medium">{{ formatBytes(trafficTotal(item)) }}</div>
                <div class="text-[11px] text-muted-foreground mt-0.5">
                  TCP {{ formatBytes(trafficBreakdown(item).tcp) }}
                </div>
                <div class="text-[11px] text-muted-foreground">
                  UDP {{ formatBytes(trafficBreakdown(item).udp) }}
                </div>
              </div>
              <div>
                <div class="text-muted-foreground mb-0.5">配额</div>
                <template v-if="item.rule.quota_bytes">
                  <div class="text-xs">{{ quotaLabel(item) }}</div>
                  <div class="mt-1.5 h-1 w-full max-w-[8rem] bg-muted overflow-hidden rounded-full">
                    <div
                      class="h-full rounded-full"
                      :class="quotaPercent(item) >= 100 ? 'bg-destructive' : 'bg-primary'"
                      :style="{ width: `${Math.min(quotaPercent(item), 100)}%` }"
                    />
                  </div>
                  <div class="text-[11px] text-muted-foreground mt-0.5 tabular-nums">{{ quotaPercent(item) }}%</div>
                </template>
                <span v-else class="text-muted-foreground">不限</span>
              </div>
            </div>

            <div class="grid grid-cols-2 gap-2 pl-7">
              <Button variant="outline" size="sm" class="w-full" @click="openEdit(item)">编辑</Button>
              <Button variant="outline" size="sm" class="w-full" @click="toggleRule(item)">
                {{ item.rule.enabled ? '停用' : '启用' }}
              </Button>
              <Button variant="outline" size="sm" class="w-full" @click="resetTraffic(item)">清零</Button>
              <Button variant="outline" size="sm" class="w-full text-destructive" @click="removeRule(item.rule.local_port)">
                删除
              </Button>
            </div>
          </article>
        </div>
      </section>
    </main>

    <div
      v-if="editing"
      class="fixed inset-0 z-40 flex items-end sm:items-center justify-center bg-black/20 p-4 pb-[max(1rem,env(safe-area-inset-bottom))]"
      @click.self="closeEdit"
    >
      <div class="w-full max-w-md max-h-[85dvh] overflow-y-auto rounded-lg border border-border bg-card p-5 shadow-sm">
        <div class="flex items-center justify-between mb-4">
          <span class="text-sm font-medium">编辑 · {{ editing.rule.local_port }}</span>
          <button type="button" class="text-xs text-muted-foreground hover:text-foreground" @click="closeEdit">关闭</button>
        </div>
        <div class="space-y-4">
          <div class="space-y-1">
            <Label>目标 IP / 域名</Label>
            <Input v-model="editTargetHost" />
          </div>
          <div class="space-y-1">
            <Label>目标端口</Label>
            <Input v-model="editTargetPort" type="number" />
          </div>
          <div class="space-y-1">
            <Label>流量配额 (GB)</Label>
            <Input v-model="editQuotaGb" type="number" step="0.1" :disabled="editUnsetQuota" placeholder="不限" />
            <label class="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer mt-2">
              <input v-model="editUnsetQuota" type="checkbox" />
              不限制流量
            </label>
          </div>
          <div v-if="!editUnsetQuota && editQuotaHasValue" class="space-y-1">
            <Label>配额周期</Label>
            <Select v-model="editQuotaPeriod">
              <option v-for="opt in quotaPeriodOptions" :key="opt.value" :value="opt.value">
                {{ opt.label }}
              </option>
            </Select>
          </div>
          <div class="flex gap-2 pt-2">
            <Button class="flex-1" :disabled="editSaving" @click="saveEdit">保存</Button>
            <Button variant="outline" class="flex-1" @click="closeEdit">取消</Button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
