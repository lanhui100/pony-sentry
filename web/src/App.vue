<template>
  <div class="min-h-screen bg-slate-950 text-slate-100 flex flex-col font-sans selection:bg-indigo-500/20 selection:text-indigo-200">
    <!-- 顶部导航栏 -->
    <header class="border-b border-slate-800/80 bg-slate-900/80 backdrop-blur-md px-6 py-4 flex items-center justify-between sticky top-0 z-40">
      <div class="flex items-center space-x-3.5">
        <div class="w-9 h-9 rounded-lg bg-gradient-to-tr from-indigo-500 via-indigo-600 to-violet-500 flex items-center justify-center font-bold text-white shadow-md shadow-indigo-500/20 text-sm tracking-tighter">
          PS
        </div>
        <div>
          <div class="flex items-center space-x-2">
            <h1 class="text-base font-semibold tracking-tight text-white">PonySentry</h1>
            <span class="text-[10px] uppercase font-mono px-1.5 py-0.5 rounded bg-slate-800 text-slate-400 border border-slate-700/60 font-medium">控制台</span>
          </div>
          <p class="text-xs text-slate-400 font-normal">轻量级多端崩溃收集与 AI 智能体缺陷诊断平台</p>
        </div>
      </div>
      <div class="flex items-center space-x-3">
        <span class="inline-flex items-center px-2.5 py-1 rounded-full text-xs font-medium bg-emerald-950/80 text-emerald-400 border border-emerald-800/80 shadow-sm shadow-emerald-950/40">
          <span class="w-1.5 h-1.5 rounded-full bg-emerald-400 mr-2 animate-pulse"></span>
          接收网关运行中
        </span>
      </div>
    </header>

    <!-- 主视口区域 -->
    <main class="flex-1 max-w-7xl w-full mx-auto p-6 space-y-5">
      <!-- 快捷过滤工具栏 -->
      <div class="flex flex-wrap items-center justify-between gap-4 bg-slate-900/50 p-4 rounded-xl border border-slate-800/80 shadow-sm backdrop-blur-sm">
        <div class="flex flex-wrap items-center gap-3">
          <!-- 状态筛选 -->
          <div class="relative">
            <select
              v-model="filterStatus"
              @change="fetchIssues"
              class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-3 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-2 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-8"
            >
              <option value="">全部状态 (All)</option>
              <option value="unresolved">待处理 (Unresolved)</option>
              <option value="in_progress">诊断/修复中 (In Progress)</option>
              <option value="resolved">已修复 (Resolved)</option>
              <option value="regression">再次复现 (Regression)</option>
              <option value="ignored">已忽略 (Ignored)</option>
            </select>
            <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center px-2 text-slate-400">
              <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"></path></svg>
            </div>
          </div>

          <!-- 平台筛选 -->
          <div class="relative">
            <select
              v-model="filterPlatform"
              @change="fetchIssues"
              class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-3 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-2 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-8"
            >
              <option value="">全部端与平台 (All)</option>
              <option value="rust">Rust 客户端</option>
              <option value="tauri">Tauri 桌面端</option>
              <option value="vue">Vue 前端</option>
              <option value="python">FastAPI 后端</option>
            </select>
            <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center px-2 text-slate-400">
              <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"></path></svg>
            </div>
          </div>

          <!-- 刷新按钮 -->
          <button
            @click="fetchIssues"
            class="bg-slate-800 hover:bg-slate-700/90 text-slate-200 px-3 py-1.5 rounded-lg text-xs font-medium border border-slate-700/70 transition flex items-center space-x-1.5 active:scale-95 shadow-sm"
          >
            <svg class="w-3.5 h-3.5 text-slate-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"></path>
            </svg>
            <span>刷新列表</span>
          </button>
        </div>

        <div class="text-xs text-slate-400 flex items-center space-x-2">
          <span>缺陷聚合总数：</span>
          <span class="font-mono font-semibold text-slate-200 bg-slate-800/80 px-2 py-0.5 rounded border border-slate-700/60">{{ issues.length }}</span>
        </div>
      </div>

      <!-- 缺陷数据表格 -->
      <div class="bg-slate-900/60 rounded-xl border border-slate-800/80 overflow-hidden shadow-xl">
        <table class="w-full text-left border-collapse">
          <thead>
            <tr class="border-b border-slate-800 text-slate-400 text-[11px] font-medium uppercase tracking-wider bg-slate-900/90">
              <th class="py-3 px-4">缺陷摘要 / 根因定位 (Issue)</th>
              <th class="py-3 px-4 w-28">上报来源</th>
              <th class="py-3 px-4 w-32">当前状态</th>
              <th class="py-3 px-4 w-24 text-center">频次</th>
              <th class="py-3 px-4 w-40">诊断修复 Agent</th>
              <th class="py-3 px-4 w-44 text-right">最后触发时间</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-slate-800/60 text-sm">
            <tr v-if="issues.length === 0">
              <td colspan="6" class="py-12 text-center text-slate-500">
                <div class="flex flex-col items-center justify-center space-y-2">
                  <svg class="w-8 h-8 text-slate-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"></path>
                  </svg>
                  <p class="text-xs text-slate-400 font-medium">暂无未解决的异常缺陷</p>
                  <p class="text-[11px] text-slate-500 font-mono">向 /api/v1/ingest 端点上报崩溃事件后将自动聚合在此</p>
                </div>
              </td>
            </tr>
            <tr v-for="issue in issues" :key="issue.id" class="hover:bg-slate-800/40 transition group">
              <!-- 标题与位置 -->
              <td class="py-3.5 px-4">
                <div
                  class="font-medium text-slate-200 group-hover:text-indigo-300 cursor-pointer transition line-clamp-1 flex items-center space-x-1.5"
                  @click="selectIssue(issue)"
                  :title="issue.title"
                >
                  <span>{{ issue.title }}</span>
                </div>
                <div class="text-xs text-slate-400 truncate max-w-lg mt-0.5 font-mono text-[11px]">
                  {{ issue.culprit || '未指定或未解析到代码栈' }}
                </div>
              </td>

              <!-- 平台 -->
              <td class="py-3.5 px-4">
                <span class="inline-flex px-2 py-0.5 rounded text-[11px] font-mono bg-slate-800 text-slate-300 border border-slate-700/60">
                  {{ issue.platform }}
                </span>
              </td>

              <!-- 状态 -->
              <td class="py-3.5 px-4">
                <span :class="statusBadgeClass(issue.status)">
                  {{ formatStatusText(issue.status) }}
                </span>
              </td>

              <!-- 频次 -->
              <td class="py-3.5 px-4 text-center font-mono text-slate-300 font-semibold text-xs">
                {{ issue.count }}
              </td>

              <!-- 修复 Agent -->
              <td class="py-3.5 px-4 text-xs font-mono">
                <span
                  v-if="issue.assigned_to"
                  :class="agentBadgeClass(issue.assigned_to)"
                >
                  {{ issue.assigned_to }}
                </span>
                <span v-else class="text-slate-500 text-[11px]">-</span>
              </td>

              <!-- 时间 -->
              <td class="py-3.5 px-4 text-right text-xs text-slate-400 font-mono text-[11px]">
                {{ formatDate(issue.last_seen_at) }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- 缺陷详情抽屉 (Modal/Drawer) -->
      <div v-if="selected" class="fixed inset-0 bg-black/60 backdrop-blur-sm flex justify-end z-50 animate-fade-in">
        <div class="w-full max-w-2xl bg-slate-900 border-l border-slate-800 h-full p-6 flex flex-col shadow-2xl overflow-hidden">
          <!-- 抽屉头部 (固定高度) -->
          <div class="flex items-start justify-between border-b border-slate-800 pb-4 shrink-0">
            <div>
              <div class="flex items-center space-x-2">
                <span :class="statusBadgeClass(selected.status)">
                  {{ formatStatusText(selected.status) }}
                </span>
                <span class="text-xs font-mono text-slate-400 bg-slate-800 px-2 py-0.5 rounded border border-slate-700">
                  {{ selected.platform }}
                </span>
              </div>
              <h2 class="text-base font-semibold text-slate-100 mt-2 break-all leading-snug">{{ selected.title }}</h2>
              <p class="text-xs text-slate-400 mt-1 font-mono">聚合唯一指纹: <code class="text-indigo-300">{{ selected.fingerprint }}</code></p>
            </div>
            <button @click="selected = null" class="text-slate-400 hover:text-slate-200 text-2xl font-light p-1 leading-none rounded-lg hover:bg-slate-800 transition">
              &times;
            </button>
          </div>

          <!-- 抽屉主体 (占据剩余空间，支持自适应布局) -->
          <div class="space-y-4 flex-1 flex flex-col min-h-0 pt-4">
            <!-- 核心出错源 (紧凑固定) -->
            <div class="shrink-0">
              <h3 class="text-xs font-medium text-slate-400 mb-1 flex items-center space-x-1">
                <span>核心出错位置 (Culprit)</span>
              </h3>
              <p class="bg-slate-950 p-2.5 rounded-lg font-mono text-xs text-rose-300 border border-slate-800/90 break-all select-all">
                {{ selected.culprit || '未获取到确切位置' }}
              </p>
            </div>

            <!-- 统计信息 (紧凑固定) -->
            <div class="grid grid-cols-4 gap-3 bg-slate-950/40 p-3 rounded-lg border border-slate-800/80 shrink-0">
              <div>
                <span class="text-[11px] text-slate-400">首次发生</span>
                <p class="text-xs font-mono text-slate-300 mt-0.5">{{ formatDate(selected.first_seen_at) }}</p>
              </div>
              <div>
                <span class="text-[11px] text-slate-400">累计发生</span>
                <p class="text-xs font-mono text-indigo-400 font-semibold mt-0.5">{{ selected.count }} 次</p>
              </div>
              <div>
                <span class="text-[11px] text-slate-400">关联版本</span>
                <p class="text-xs font-mono text-slate-300 mt-0.5">{{ selected.last_release || '未知版本' }}</p>
              </div>
              <div>
                <span class="text-[11px] text-slate-400">诊断修复 Agent</span>
                <p class="text-xs font-mono mt-0.5">
                  <span v-if="selected.assigned_to" :class="agentBadgeClass(selected.assigned_to)">
                    {{ selected.assigned_to }}
                  </span>
                  <span v-else class="text-slate-500">未指派</span>
                </p>
              </div>
            </div>

            <!-- 事件明细 (占满所有剩余高度，超出视口可滚动且隐藏滚动条) -->
            <div class="flex-1 flex flex-col min-h-0">
              <div class="flex items-center justify-between mb-2 shrink-0">
                <h3 class="text-xs font-medium text-slate-400">近期原始上报事件 (Recent Events)</h3>
                <span class="text-[11px] text-slate-500 font-mono">已脱敏保留</span>
              </div>
              <div class="flex-1 overflow-y-auto no-scrollbar space-y-3 pr-0.5">
                <div v-for="ev in selectedEvents" :key="ev.id" class="bg-slate-950 p-3.5 rounded-lg border border-slate-800 text-xs space-y-2">
                  <div class="flex justify-between items-center text-slate-400 border-b border-slate-800/60 pb-1.5">
                    <span class="font-mono text-[11px]">事件ID: {{ ev.id }}</span>
                    <span class="font-mono text-[11px]">{{ formatDate(ev.created_at) }}</span>
                  </div>
                  <pre class="overflow-x-auto no-scrollbar text-slate-300 p-2.5 bg-slate-900/70 rounded border border-slate-800/50 font-mono text-[11px] leading-relaxed">{{ JSON.stringify(ev.payload, null, 2) }}</pre>
                </div>
              </div>
            </div>
          </div>

          <!-- 抽屉底部操作 (固定底部) -->
          <div class="pt-4 border-t border-slate-800 flex justify-between items-center shrink-0">
            <span class="text-xs text-slate-500 font-mono flex items-center space-x-1.5">
              <span class="w-1.5 h-1.5 rounded-full bg-indigo-500 animate-pulse"></span>
              <span>由 Coding Agent 自动诊断闭环</span>
            </span>
            <button
              @click="selected = null"
              class="text-xs bg-slate-800 hover:bg-slate-700 text-slate-300 font-medium px-4 py-2 rounded-lg transition active:scale-95"
            >
              关闭
            </button>
          </div>
        </div>
      </div>
    </main>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue'

const issues = ref([])
const selected = ref(null)
const selectedEvents = ref([])
const filterStatus = ref('')
const filterPlatform = ref('')

const fetchIssues = async () => {
  let url = '/api/v1/issues?'
  if (filterStatus.value) url += `status=${filterStatus.value}&`
  if (filterPlatform.value) url += `platform=${filterPlatform.value}&`
  try {
    const res = await fetch(url)
    if (res.ok) {
      issues.value = await res.json()
    }
  } catch (e) {
    console.error('加载缺陷列表失败', e)
  }
}

const updateStatus = async (id, status) => {
  try {
    const res = await fetch(`/api/v1/issues/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status })
    })
    if (res.ok) {
      fetchIssues()
    }
  } catch (e) {
    console.error('更新缺陷状态失败', e)
  }
}

const selectIssue = async (issue) => {
  selected.value = issue
  try {
    const res = await fetch(`/api/v1/issues/${issue.id}/events`)
    if (res.ok) {
      selectedEvents.value = await res.json()
    }
  } catch (e) {
    console.error('获取缺陷事件详情失败', e)
  }
}

const formatDate = (isoStr) => {
  if (!isoStr) return '-'
  try {
    return new Date(isoStr).toLocaleString('zh-CN', {
      hour12: false,
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit'
    })
  } catch {
    return isoStr
  }
}

const formatStatusText = (status) => {
  switch (status) {
    case 'unresolved':
      return '待处理'
    case 'in_progress':
      return '修复中'
    case 'resolved':
      return '已解决'
    case 'regression':
      return '再次复现'
    case 'ignored':
      return '已忽略'
    default:
      return status
  }
}

const statusBadgeClass = (status) => {
  switch (status) {
    case 'unresolved':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-rose-950 text-rose-400 border border-rose-800'
    case 'in_progress':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-amber-950 text-amber-400 border border-amber-800'
    case 'resolved':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-emerald-950 text-emerald-400 border border-emerald-800'
    case 'regression':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-purple-950 text-purple-400 border border-purple-800'
    default:
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-slate-800 text-slate-300'
  }
}

const agentBadgeClass = (agent) => {
  const name = (agent || '').toLowerCase()
  if (name.includes('dsh')) {
    return 'inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-indigo-950 text-indigo-300 border border-indigo-800/80'
  } else if (name.includes('codex') || name.includes('openai')) {
    return 'inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-emerald-950 text-emerald-300 border border-emerald-800/80'
  } else if (name.includes('claude') || name.includes('anthropic')) {
    return 'inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-amber-950 text-amber-300 border border-amber-800/80'
  } else if (name.includes('opencode')) {
    return 'inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-cyan-950 text-cyan-300 border border-cyan-800/80'
  }
  return 'inline-flex items-center px-2 py-0.5 rounded text-[11px] font-mono font-medium bg-slate-800 text-slate-300 border border-slate-700/60'
}

onMounted(() => {
  fetchIssues()
})
</script>
