<template>
  <div class="min-h-screen bg-slate-950 text-slate-100 flex flex-col font-sans selection:bg-indigo-500/30 selection:text-indigo-200">
    <!-- 顶部极简导航栏 -->
    <header class="border-b border-slate-800/80 bg-slate-900/90 backdrop-blur-md px-6 py-4 flex items-center justify-between sticky top-0 z-40">
      <div class="flex items-center space-x-6">
        <div class="flex items-center space-x-3.5">
          <div class="w-9 h-9 rounded-lg bg-gradient-to-tr from-indigo-500 via-indigo-600 to-violet-500 flex items-center justify-center font-bold text-white shadow-md shadow-indigo-500/20 text-sm tracking-tight font-mono">
            PS
          </div>
          <div>
            <div class="flex items-center space-x-2">
              <h1 class="text-base font-semibold tracking-tight text-white">PonySentry</h1>
              <span class="text-[11px] uppercase font-mono px-1.5 py-0.5 rounded bg-slate-800/80 text-slate-400 border border-slate-700/60 font-medium">Console</span>
            </div>
            <p class="text-xs text-slate-400 font-normal">多端崩溃遥测与 AI 缺陷诊断</p>
          </div>
        </div>

        <!-- 主视图模式切换 Tabs -->
        <nav class="flex items-center space-x-1 bg-slate-950/80 p-1 rounded-xl border border-slate-800">
          <button
            @click="currentView = 'issues'"
            class="px-3 py-1.5 rounded-lg text-xs font-medium transition flex items-center space-x-1.5"
            :class="currentView === 'issues' ? 'bg-indigo-600 text-white shadow-sm' : 'text-slate-400 hover:text-slate-200 hover:bg-slate-900'"
          >
            <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"/></svg>
            <span>崩溃遥测 (Issues)</span>
          </button>
          <button
            @click="currentView = 'traces'"
            class="px-3 py-1.5 rounded-lg text-xs font-medium transition flex items-center space-x-1.5"
            :class="currentView === 'traces' ? 'bg-indigo-600 text-white shadow-sm' : 'text-slate-400 hover:text-slate-200 hover:bg-slate-900'"
          >
            <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 19v-6a2 2 0 00-2-2H5a2 2 0 00-2 2v6a2 2 0 002 2h2a2 2 0 002-2zm0 0V9a2 2 0 012-2h2a2 2 0 012 2v10m-6 0a2 2 0 002 2h2a2 2 0 002-2m0 0V5a2 2 0 012-2h2a2 2 0 012 2v14a2 2 0 01-2 2h-2a2 2 0 01-2-2z"/></svg>
            <span>Agent Trace 调优 (Traces)</span>
          </button>
        </nav>
      </div>
      <div class="flex items-center space-x-3">
        <!-- 自动刷新状态提示 -->
        <span class="inline-flex items-center px-3 py-1 rounded-full text-xs font-medium bg-emerald-950/80 text-emerald-400 border border-emerald-800/80 shadow-sm shadow-emerald-950/30">
          <span class="w-1.5 h-1.5 rounded-full bg-emerald-400 mr-2 animate-pulse"></span>
          网关就绪
        </span>
        <button
          @click="refreshAll"
          class="bg-slate-800 hover:bg-slate-700 text-slate-200 px-3 py-1.5 rounded-lg text-xs font-medium border border-slate-700/70 transition flex items-center space-x-1.5 active:scale-95 shadow-sm"
          title="快捷键: R"
        >
          <svg class="w-4 h-4 text-slate-400" :class="{ 'animate-spin': isRefreshing }" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"></path>
          </svg>
          <span>刷新</span>
        </button>
      </div>
    </header>

    <!-- 主视口区域 -->
    <main class="flex-1 max-w-7xl w-full mx-auto p-6 space-y-5">
      <!-- 视图 1：Issues 崩溃遥测 -->
      <section v-if="currentView === 'issues'" class="space-y-5">
      <!-- 统计指标与健康概览卡片 (KPI Bento) -->
      <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
        <div
          @click="filterStatus = 'attention'"
          class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-indigo-500/50 transition"
          :class="{ 'ring-2 ring-indigo-500/40 bg-slate-900/90': filterStatus === 'attention' }"
        >
          <div class="text-xs uppercase tracking-wider text-slate-300 font-medium">需关注缺陷 (默认)</div>
          <div class="mt-2 flex items-baseline justify-between">
            <span class="text-2xl font-bold font-mono text-indigo-300">{{ attentionCount }}</span>
            <span class="text-xs text-slate-400 font-mono">待处理+诊断中</span>
          </div>
        </div>
        <div
          @click="filterStatus = 'unresolved'"
          class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-rose-500/50 transition"
          :class="{ 'ring-2 ring-rose-500/40 bg-slate-900/90': filterStatus === 'unresolved' }"
        >
          <div class="text-xs uppercase tracking-wider text-rose-400 font-medium">待处理 (Unresolved)</div>
          <div class="mt-2 flex items-baseline justify-between">
            <span class="text-2xl font-bold font-mono text-rose-300">{{ unresolvedCount }}</span>
            <span class="text-xs text-rose-400/80 font-mono">紧急</span>
          </div>
        </div>
        <div
          @click="filterStatus = 'in_progress'"
          class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-amber-500/50 transition"
          :class="{ 'ring-2 ring-amber-500/40 bg-slate-900/90': filterStatus === 'in_progress' }"
        >
          <div class="text-xs uppercase tracking-wider text-amber-400 font-medium">诊断修复中</div>
          <div class="mt-2 flex items-baseline justify-between">
            <span class="text-2xl font-bold font-mono text-amber-300">{{ inProgressCount }}</span>
            <span class="text-xs text-amber-400/80 font-mono">In Progress</span>
          </div>
        </div>
        <div
          @click="filterStatus = 'resolved'"
          class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-emerald-500/50 transition"
          :class="{ 'ring-2 ring-emerald-500/40 bg-slate-900/90': filterStatus === 'resolved' }"
        >
          <div class="text-xs uppercase tracking-wider text-emerald-400 font-medium">已修复 (Resolved)</div>
          <div class="mt-2 flex items-baseline justify-between">
            <span class="text-2xl font-bold font-mono text-emerald-300">{{ resolvedCount }}</span>
            <span class="text-xs text-emerald-400/80 font-mono">历史闭环</span>
          </div>
        </div>
      </div>

      <!-- 快捷过滤工具栏与关键字搜索 -->
      <div class="flex flex-wrap items-center justify-between gap-4 bg-slate-900/50 p-3.5 rounded-xl border border-slate-800/80 shadow-sm backdrop-blur-sm">
        <div class="flex flex-wrap items-center gap-2.5 flex-1 max-w-3xl">
          <!-- 关键词快速搜索 -->
          <div class="relative flex-1 min-w-[200px]">
            <input
              type="text"
              v-model="searchKeyword"
              placeholder="搜索缺陷标题、代码路径、指纹..."
              class="w-full bg-slate-800/80 border border-slate-700/80 rounded-lg pl-8 pr-3 py-1.5 text-xs text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition"
            />
            <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5 text-slate-500">
              <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"></path></svg>
            </div>
            <button
              v-if="searchKeyword"
              @click="searchKeyword = ''"
              class="absolute inset-y-0 right-0 flex items-center pr-2 text-slate-400 hover:text-slate-200 text-xs"
            >&times;</button>
          </div>

          <!-- 状态筛选 -->
          <div class="relative">
            <select
              v-model="filterStatus"
              class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-3 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-8 font-medium"
            >
              <option value="attention">需关注问题 (默认)</option>
              <option value="unresolved">待处理 (Unresolved)</option>
              <option value="in_progress">诊断修复中 (In Progress)</option>
              <option value="regression">再次复现 (Regression)</option>
              <option value="resolved">已修复 (Resolved)</option>
              <option value="ignored">已忽略 (Ignored)</option>
              <option value="">全部状态 (All)</option>
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
              class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-2.5 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-7"
            >
              <option value="">全部平台</option>
              <option value="rust">Rust 客户端</option>
              <option value="tauri">Tauri 桌面端</option>
              <option value="vue">Vue 前端</option>
              <option value="python">FastAPI 后端</option>
            </select>
            <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center px-2 text-slate-400">
              <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"></path></svg>
            </div>
          </div>

          <!-- 项目筛选 -->
          <div class="relative">
            <select
              v-model="filterProject"
              @change="fetchIssues"
              class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-2.5 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-7"
            >
              <option value="">全部项目</option>
              <option v-for="p in projects" :key="p" :value="p">{{ p }}</option>
            </select>
            <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center px-2 text-slate-400">
              <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"></path></svg>
            </div>
          </div>
        </div>

        <div class="text-xs text-slate-400 flex items-center space-x-2 shrink-0">
          <span>匹配缺陷：</span>
          <span class="font-mono font-semibold text-slate-200 bg-slate-800/90 px-2 py-0.5 rounded border border-slate-700/60">
            {{ filteredIssues.length }}
          </span>
        </div>
      </div>

      <!-- 缺陷数据表格 -->
      <div class="bg-slate-900/60 rounded-xl border border-slate-800/80 overflow-hidden shadow-xl">
        <div class="overflow-x-auto">
          <table class="w-full text-left border-collapse table-fixed">
            <thead>
              <tr class="border-b border-slate-800 text-slate-300 text-xs font-semibold uppercase tracking-wider bg-slate-900/90">
                <th class="py-3.5 px-4 w-[42%]">缺陷摘要 / 根因定位 (Issue)</th>
                <th class="py-3.5 px-4 w-[10%]">来源</th>
                <th class="py-3.5 px-4 w-[12%]">项目</th>
                <th class="py-3.5 px-4 w-[10%]">状态</th>
                <th class="py-3.5 px-4 w-[7%] text-center">频次</th>
                <th class="py-3.5 px-4 w-[11%]">诊断修复</th>
                <th class="py-3.5 px-4 w-[12%] text-right">最后上报</th>
              </tr>
            </thead>
            <tbody class="divide-y divide-slate-800/60 text-sm">
              <tr v-if="filteredIssues.length === 0">
                <td colspan="7" class="py-14 text-center text-slate-500">
                  <div class="flex flex-col items-center justify-center space-y-2.5">
                    <svg class="w-9 h-9 text-slate-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"></path>
                    </svg>
                    <p class="text-sm text-slate-400 font-medium">无匹配的异常缺陷</p>
                    <p class="text-xs text-slate-500 font-mono">当前视图仅展示需关注的待处理/诊断中缺陷，可切换全部状态查看</p>
                  </div>
                </td>
              </tr>
              <tr
                v-for="issue in filteredIssues"
                :key="issue.id"
                class="hover:bg-slate-800/40 transition group cursor-pointer"
                @click="selectIssue(issue)"
              >
                <!-- 标题与位置：严格限制单行截断（truncate + max-w-full）避免折行撑高表格 -->
                <td class="py-3 px-4 overflow-hidden max-w-0">
                  <div
                    class="font-medium text-slate-100 group-hover:text-indigo-300 transition truncate text-sm leading-snug"
                    :title="issue.title"
                  >
                    {{ issue.title }}
                  </div>
                  <div class="text-xs text-slate-400 truncate mt-1 font-mono">
                    {{ issue.culprit || '未指定代码栈位置' }}
                  </div>
                </td>

                <!-- 平台 -->
                <td class="py-3 px-4 truncate">
                  <span class="inline-flex px-2 py-0.5 rounded text-xs font-mono bg-slate-800 text-slate-300 border border-slate-700/60">
                    {{ issue.platform }}
                  </span>
                </td>

                <!-- 项目 -->
                <td class="py-3 px-4 truncate">
                  <span
                    v-if="issue.project"
                    class="font-mono text-xs text-indigo-300/90 font-medium"
                    :title="issue.project"
                  >{{ issue.project }}</span>
                  <span v-else class="text-slate-600 text-xs font-mono">未上报</span>
                </td>

                <!-- 状态 -->
                <td class="py-3 px-4 truncate">
                  <span :class="statusBadgeClass(issue.status)">
                    {{ formatStatusText(issue.status) }}
                  </span>
                </td>

                <!-- 频次 -->
                <td class="py-3 px-4 text-center font-mono text-slate-200 font-semibold text-xs">
                  {{ issue.count }}
                </td>

                <!-- 诊断修复 (去除 Agent 字样) -->
                <td class="py-3 px-4 text-xs font-mono truncate">
                  <span
                    v-if="issue.assigned_to"
                    :class="agentBadgeClass(issue.assigned_to)"
                  >
                    {{ issue.assigned_to }}
                  </span>
                  <span v-else class="text-slate-500 text-xs">-</span>
                </td>

                <!-- 时间 -->
                <td class="py-3 px-4 text-right text-xs text-slate-400 font-mono">
                  {{ formatDate(issue.last_seen_at) }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- 缺陷详情抽屉 (Modal/Drawer) -->
      <div
        v-if="selected"
        class="fixed inset-0 bg-black/60 backdrop-blur-sm flex justify-end z-50 transition-opacity"
        @keydown.esc="selected = null"
      >
        <div class="w-full max-w-2xl bg-slate-900 border-l border-slate-800 h-full p-6 flex flex-col shadow-2xl overflow-hidden animate-slide-in">
          <!-- 抽屉头部 -->
          <div class="flex items-start justify-between border-b border-slate-800 pb-4 shrink-0">
            <div>
              <div class="flex items-center space-x-2">
                <span :class="statusBadgeClass(selected.status)">
                  {{ formatStatusText(selected.status) }}
                </span>
                <span class="text-xs font-mono text-slate-400 bg-slate-800 px-2 py-0.5 rounded border border-slate-700">
                  {{ selected.platform }}
                </span>
                <span v-if="selected.project" class="text-xs font-mono text-indigo-300 bg-indigo-950/60 px-2 py-0.5 rounded border border-indigo-800/60">
                  {{ selected.project }}
                </span>
              </div>
              <h2 class="text-base font-semibold text-slate-100 mt-2 break-all leading-snug">{{ selected.title }}</h2>
              <div class="flex items-center space-x-2 mt-1">
                <p class="text-xs text-slate-400 font-mono">指纹: <code class="text-indigo-300">{{ selected.fingerprint }}</code></p>
                <button
                  @click="copyText(selected.fingerprint, '指纹')"
                  class="text-[10px] text-slate-400 hover:text-slate-200 border border-slate-700 px-1.5 py-0.2 rounded hover:bg-slate-800 transition"
                >
                  {{ copySuccess === '指纹' ? '已复制' : '复制' }}
                </button>
              </div>
            </div>
            <button
              @click="selected = null"
              class="text-slate-400 hover:text-slate-200 text-2xl font-light p-1 leading-none rounded-lg hover:bg-slate-800 transition"
              title="Esc 键关闭"
            >
              &times;
            </button>
          </div>

          <!-- 快速流转操作栏 -->
          <div class="flex items-center justify-between bg-slate-800/40 px-3.5 py-2.5 border-b border-slate-800 shrink-0 text-xs">
            <span class="text-slate-300 font-medium">状态标记：</span>
            <div class="flex items-center space-x-2">
              <button
                @click="updateStatus(selected.id, 'unresolved')"
                :disabled="selected.status === 'unresolved'"
                class="px-2.5 py-1 rounded border text-xs font-medium transition"
                :class="selected.status === 'unresolved' ? 'bg-rose-950/60 border-rose-800 text-rose-300' : 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700'"
              >
                待处理
              </button>
              <button
                @click="updateStatus(selected.id, 'in_progress')"
                :disabled="selected.status === 'in_progress'"
                class="px-2.5 py-1 rounded border text-xs font-medium transition"
                :class="selected.status === 'in_progress' ? 'bg-amber-950/60 border-amber-800 text-amber-300' : 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700'"
              >
                诊断中
              </button>
              <button
                @click="updateStatus(selected.id, 'resolved')"
                :disabled="selected.status === 'resolved'"
                class="px-2.5 py-1 rounded border text-xs font-medium transition"
                :class="selected.status === 'resolved' ? 'bg-emerald-950/60 border-emerald-800 text-emerald-300' : 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700'"
              >
                标记已解决
              </button>
              <button
                @click="updateStatus(selected.id, 'ignored')"
                :disabled="selected.status === 'ignored'"
                class="px-2.5 py-1 rounded border text-xs font-medium transition"
                :class="selected.status === 'ignored' ? 'bg-slate-700 border-slate-600 text-slate-300' : 'bg-slate-800 border-slate-700 text-slate-400 hover:bg-slate-700'"
              >
                忽略
              </button>
            </div>
          </div>

          <!-- 抽屉主体内容区 -->
          <div class="space-y-5 flex-1 flex flex-col min-h-0 pt-4">
            <!-- 核心出错源 -->
            <div class="shrink-0">
              <div class="flex items-center justify-between">
                <h3 class="text-xs uppercase tracking-widest font-semibold text-slate-400">
                  核心出错位置 · Culprit
                </h3>
                <button
                  v-if="selected.culprit"
                  @click="copyText(selected.culprit, 'Culprit')"
                  class="text-xs text-slate-400 hover:text-slate-200 border border-slate-700 px-2 py-0.5 rounded hover:bg-slate-800 transition"
                >
                  {{ copySuccess === 'Culprit' ? '已复制' : '复制栈位置' }}
                </button>
              </div>
              <p class="mt-2 font-mono text-xs leading-relaxed text-rose-300/90 break-all select-all bg-rose-950/20 p-3 rounded-lg border border-rose-900/40">
                {{ selected.culprit || '未获取到确切代码栈位置' }}
              </p>
            </div>

            <!-- 核心元数据统计面板 -->
            <div class="grid grid-cols-4 gap-3 bg-slate-900/80 p-3.5 rounded-lg border border-slate-800/80 shrink-0">
              <div>
                <span class="block text-xs text-slate-400">首次发生</span>
                <p class="mt-1 text-xs font-mono text-slate-300">{{ formatDate(selected.first_seen_at) }}</p>
              </div>
              <div>
                <span class="block text-xs text-slate-400">累计频次</span>
                <p class="mt-1 text-base font-mono text-indigo-300 font-semibold">{{ selected.count }}</p>
              </div>
              <div>
                <span class="block text-xs text-slate-400">发布版本</span>
                <p class="mt-1 text-xs font-mono text-slate-300">{{ selected.last_release || '未指定' }}</p>
              </div>
              <div>
                <span class="block text-xs text-slate-400">诊断修复</span>
                <p class="mt-1 text-xs font-mono text-slate-300 truncate">
                  <span v-if="selected.assigned_to">{{ selected.assigned_to }}</span>
                  <span v-else class="text-slate-500">未指派</span>
                </p>
              </div>
            </div>

            <!-- 事件明细列表 -->
            <div class="flex-1 flex flex-col min-h-0">
              <div class="flex items-baseline justify-between mb-2 shrink-0">
                <h3 class="text-xs uppercase tracking-widest font-semibold text-slate-400">
                  原始上报事件 ({{ selectedEvents.length }})
                </h3>
                <span class="text-xs text-slate-500 font-mono">已脱敏处理</span>
              </div>
              <div class="flex-1 overflow-y-auto no-scrollbar space-y-4 pr-1">
                <p v-if="selectedEvents.length === 0" class="py-8 text-center text-xs text-slate-500">
                  正在加载或暂无事件明细
                </p>
                <article
                  v-for="ev in selectedEvents"
                  :key="ev.id"
                  class="bg-slate-950/60 p-3.5 rounded-lg border border-slate-800/80 space-y-2.5"
                >
                  <div class="flex justify-between items-baseline text-xs font-mono text-slate-400 border-b border-slate-800/60 pb-1.5">
                    <span class="font-semibold text-slate-300">事件 #{{ ev.id }}</span>
                    <span>{{ formatDate(ev.created_at) }}</span>
                  </div>

                  <!-- 面包屑轻量流式展现 -->
                  <div v-if="ev.payload && ev.payload.breadcrumbs && ev.payload.breadcrumbs.length" class="space-y-1">
                    <div class="text-xs uppercase font-semibold text-slate-300">Breadcrumbs 行为轨迹:</div>
                    <div class="bg-slate-900/80 p-2.5 rounded border border-slate-800/60 space-y-1 max-h-36 overflow-y-auto no-scrollbar font-mono text-xs">
                      <div
                        v-for="(b, idx) in ev.payload.breadcrumbs"
                        :key="idx"
                        class="flex items-center space-x-2 text-slate-300"
                      >
                        <span class="text-slate-500 text-xs">{{ idx + 1 }}.</span>
                        <span class="text-indigo-400 font-medium">[{{ b.category || 'default' }}]</span>
                        <span class="truncate">{{ b.message || JSON.stringify(b.data || {}) }}</span>
                      </div>
                    </div>
                  </div>

                  <!-- 原始 Payload -->
                  <div>
                    <div class="text-xs uppercase font-semibold text-slate-300 mb-1">Payload 结构:</div>
                    <pre class="bg-slate-900/90 p-2.5 rounded border border-slate-800/80 font-mono text-xs leading-relaxed text-slate-300 whitespace-pre-wrap break-all max-h-52 overflow-y-auto no-scrollbar">{{ JSON.stringify(ev.payload, null, 2) }}</pre>
                  </div>
                </article>
              </div>
            </div>
          </div>

          <!-- 抽屉底部操作 -->
          <div class="pt-3.5 border-t border-slate-800 flex justify-between items-center shrink-0">
            <span class="text-xs text-slate-500 font-mono flex items-center space-x-1.5">
              <span class="w-1.5 h-1.5 rounded-full bg-indigo-500 animate-pulse"></span>
              <span>由 Coding Agent 自动闭环治理</span>
            </span>
            <button
              @click="selected = null"
              class="text-xs bg-slate-800 hover:bg-slate-700 text-slate-300 font-medium px-4 py-1.5 rounded-lg transition active:scale-95"
            >
              关闭 (Esc)
            </button>
          </div>
        </div>
      </div>
      </section>

      <!-- 视图 2：Agent Trace 调优与评估看板 -->
      <section v-else-if="currentView === 'traces'" class="space-y-5">
        <!-- Trace KPI 统计卡片 -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div
            @click="filterTraceStatus = 'unreviewed'"
            class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-amber-500/50 transition"
            :class="{ 'ring-2 ring-amber-500/40 bg-slate-900/90': filterTraceStatus === 'unreviewed' }"
          >
            <div class="text-xs uppercase tracking-wider text-amber-400 font-medium">待评估 (Unreviewed)</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-bold font-mono text-amber-300">{{ traceUnreviewedCount }}</span>
              <span class="text-xs text-amber-400/80 font-mono">待标注</span>
            </div>
          </div>
          <div
            @click="filterTraceStatus = 'triage_good'"
            class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-emerald-500/50 transition"
            :class="{ 'ring-2 ring-emerald-500/40 bg-slate-900/90': filterTraceStatus === 'triage_good' }"
          >
            <div class="text-xs uppercase tracking-wider text-emerald-400 font-medium">标杆样本 (Good)</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-bold font-mono text-emerald-300">{{ traceGoodCount }}</span>
              <span class="text-xs text-emerald-400/80 font-mono">高质量</span>
            </div>
          </div>
          <div
            @click="filterTraceStatus = 'triage_bad'"
            class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-rose-500/50 transition"
            :class="{ 'ring-2 ring-rose-500/40 bg-slate-900/90': filterTraceStatus === 'triage_bad' }"
          >
            <div class="text-xs uppercase tracking-wider text-rose-400 font-medium">缺陷样本 (Bad)</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-bold font-mono text-rose-300">{{ traceBadCount }}</span>
              <span class="text-xs text-rose-400/80 font-mono">幻觉/报错</span>
            </div>
          </div>
          <div
            @click="filterTraceStatus = 'optimized'"
            class="bg-slate-900/60 p-4 rounded-xl border border-slate-800/80 backdrop-blur-sm flex flex-col justify-between cursor-pointer hover:border-indigo-500/50 transition"
            :class="{ 'ring-2 ring-indigo-500/40 bg-slate-900/90': filterTraceStatus === 'optimized' }"
          >
            <div class="text-xs uppercase tracking-wider text-indigo-400 font-medium">已完成优化 (Optimized)</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-bold font-mono text-indigo-300">{{ traceOptimizedCount }}</span>
              <span class="text-xs text-indigo-400/80 font-mono">Prompt/微调已闭环</span>
            </div>
          </div>
        </div>

        <!-- Trace 过滤工具栏 -->
        <div class="flex flex-wrap items-center justify-between gap-4 bg-slate-900/50 p-3.5 rounded-xl border border-slate-800/80 shadow-sm backdrop-blur-sm">
          <div class="flex flex-wrap items-center gap-2.5 flex-1 max-w-3xl">
            <!-- 搜索框 -->
            <div class="relative flex-1 min-w-[200px]">
              <input
                type="text"
                v-model="traceSearchKeyword"
                placeholder="搜索 Session ID, Run ID, 环境..."
                class="w-full bg-slate-800/80 border border-slate-700/80 rounded-lg pl-8 pr-3 py-1.5 text-xs text-slate-200 placeholder-slate-500 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition"
              />
              <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5 text-slate-500">
                <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"></path></svg>
              </div>
              <button
                v-if="traceSearchKeyword"
                @click="traceSearchKeyword = ''"
                class="absolute inset-y-0 right-0 flex items-center pr-2 text-slate-400 hover:text-slate-200 text-xs"
              >&times;</button>
            </div>

            <!-- 状态快速筛选 -->
            <div class="relative">
              <select
                v-model="filterTraceStatus"
                class="bg-slate-800/90 hover:bg-slate-800 border border-slate-700/80 rounded-lg px-3 py-1.5 text-xs text-slate-200 focus:outline-none focus:ring-1.5 focus:ring-indigo-500 transition cursor-pointer appearance-none pr-8 font-medium"
              >
                <option value="">全部状态 (All Traces)</option>
                <option value="unreviewed">待评估 (Unreviewed)</option>
                <option value="triage_good">标杆样本 (Triage Good)</option>
                <option value="triage_bad">缺陷样本 (Triage Bad)</option>
                <option value="eval_dataset">评测集 (Eval Dataset)</option>
                <option value="optimized">已优化 (Optimized)</option>
                <option value="wontfix">已放弃 (Wontfix)</option>
              </select>
              <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center px-2 text-slate-400">
                <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 9l-7 7-7-7"></path></svg>
              </div>
            </div>
          </div>

          <div class="text-xs text-slate-400 flex items-center space-x-2 shrink-0">
            <span>共展示 <strong class="text-slate-200 font-mono">{{ filteredTraces.length }}</strong> 条 Trace 轨迹</span>
          </div>
        </div>

        <!-- Trace 列表展示 -->
        <div class="bg-slate-900/50 rounded-xl border border-slate-800/80 overflow-hidden shadow-sm backdrop-blur-sm">
          <div v-if="filteredTraces.length === 0" class="py-16 text-center text-slate-500 text-xs">
            暂无匹配的 Agent Trace 遥测记录
          </div>
          <div v-else class="divide-y divide-slate-800/70">
            <div
              v-for="trace in filteredTraces"
              :key="trace.id"
              @click="openTraceDetail(trace)"
              class="p-4.5 hover:bg-slate-800/40 transition cursor-pointer flex flex-col md:flex-row md:items-center justify-between gap-4 border-l-2"
              :class="traceBorderClass(trace.eval_status)"
            >
              <div class="space-y-1.5 flex-1 min-w-0">
                <div class="flex items-center space-x-2.5">
                  <span class="font-mono text-sm font-semibold text-slate-200 truncate">{{ trace.session_id }}</span>
                  <span :class="evalBadgeClass(trace.eval_status)">
                    {{ formatEvalStatusText(trace.eval_status) }}
                  </span>
                  <span class="text-[11px] font-mono px-1.5 py-0.5 rounded bg-slate-800 text-slate-400 border border-slate-700/60">
                    {{ trace.environment }}
                  </span>
                  <span class="text-[11px] font-mono text-slate-500">
                    {{ trace.release }}
                  </span>
                </div>
                <div class="flex items-center space-x-4 text-xs text-slate-400 font-mono">
                  <span>Turns: <strong class="text-slate-300">{{ getTraceTurns(trace).length }}</strong></span>
                  <span>Tokens: <strong class="text-slate-300">{{ (trace.total_input_tokens || 0) + (trace.total_output_tokens || 0) }}</strong> (in: {{ trace.total_input_tokens || 0 }}, out: {{ trace.total_output_tokens || 0 }})</span>
                  <span>耗时: <strong class="text-slate-300">{{ trace.total_duration_ms || 0 }}ms</strong></span>
                </div>
              </div>
              <div class="text-right shrink-0 font-mono text-xs text-slate-500">
                <div>{{ formatDate(trace.reported_at || trace.created_at) }}</div>
              </div>
            </div>
          </div>
        </div>

        <!-- Trace 详情抽屉模态框 -->
        <div
          v-if="selectedTrace"
          class="fixed inset-0 z-50 flex items-center justify-end bg-slate-950/70 backdrop-blur-sm"
          @click.self="selectedTrace = null"
        >
          <div class="w-full max-w-3xl h-full bg-slate-900 border-l border-slate-800 p-6 flex flex-col space-y-4 shadow-2xl overflow-hidden">
            <!-- 头部 -->
            <div class="flex items-start justify-between pb-4 border-b border-slate-800 shrink-0">
              <div class="space-y-1">
                <div class="flex items-center space-x-2.5">
                  <h2 class="text-base font-semibold text-white font-mono">{{ selectedTrace.session_id }}</h2>
                  <span :class="evalBadgeClass(selectedTrace.eval_status)">
                    {{ formatEvalStatusText(selectedTrace.eval_status) }}
                  </span>
                </div>
                <p class="text-xs text-slate-400 font-mono">
                  Trace ID: {{ selectedTrace.id }} | 报告时间: {{ formatDate(selectedTrace.reported_at) }}
                </p>
              </div>
              <button
                @click="selectedTrace = null"
                class="text-slate-400 hover:text-slate-200 text-lg p-1 rounded-lg hover:bg-slate-800"
              >&times;</button>
            </div>

            <!-- 状态快速打标流转操作栏 -->
            <div class="bg-slate-950/60 p-3 rounded-lg border border-slate-800/80 flex items-center justify-between shrink-0">
              <span class="text-xs text-slate-400 font-medium">评估与数据对齐标注:</span>
              <div class="flex items-center space-x-1.5 flex-wrap gap-1">
                <button
                  @click="updateTraceStatus(selectedTrace.id, 'triage_good')"
                  class="px-2.5 py-1 text-xs rounded font-medium border border-emerald-800/80 bg-emerald-950/60 text-emerald-300 hover:bg-emerald-900/60 transition"
                  :class="{ 'ring-2 ring-emerald-500': selectedTrace.eval_status === 'triage_good' }"
                >标杆 (Good)</button>
                <button
                  @click="updateTraceStatus(selectedTrace.id, 'triage_bad')"
                  class="px-2.5 py-1 text-xs rounded font-medium border border-rose-800/80 bg-rose-950/60 text-rose-300 hover:bg-rose-900/60 transition"
                  :class="{ 'ring-2 ring-rose-500': selectedTrace.eval_status === 'triage_bad' }"
                >缺陷 (Bad)</button>
                <button
                  @click="updateTraceStatus(selectedTrace.id, 'eval_dataset')"
                  class="px-2.5 py-1 text-xs rounded font-medium border border-purple-800/80 bg-purple-950/60 text-purple-300 hover:bg-purple-900/60 transition"
                  :class="{ 'ring-2 ring-purple-500': selectedTrace.eval_status === 'eval_dataset' }"
                >评测集</button>
                <button
                  @click="updateTraceStatus(selectedTrace.id, 'optimized')"
                  class="px-2.5 py-1 text-xs rounded font-medium border border-indigo-800/80 bg-indigo-950/60 text-indigo-300 hover:bg-indigo-900/60 transition"
                  :class="{ 'ring-2 ring-indigo-500': selectedTrace.eval_status === 'optimized' }"
                >已优化</button>
                <button
                  @click="updateTraceStatus(selectedTrace.id, 'wontfix')"
                  class="px-2.5 py-1 text-xs rounded font-medium border border-slate-700 bg-slate-800 text-slate-400 hover:bg-slate-700 transition"
                  :class="{ 'ring-2 ring-slate-500': selectedTrace.eval_status === 'wontfix' }"
                >已放弃</button>
              </div>
            </div>

            <!-- Turns 展开明细 -->
            <div class="flex-1 overflow-y-auto space-y-3.5 pr-1 no-scrollbar text-xs">
              <div class="font-semibold text-slate-300 uppercase tracking-wider text-xs">执行轮次轨迹 (Turns):</div>
              <div v-if="getTraceTurns(selectedTrace).length === 0" class="text-slate-500 py-6 text-center">
                该 Trace 无 turns 明细
              </div>
              <div
                v-for="(turn, idx) in getTraceTurns(selectedTrace)"
                :key="idx"
                class="bg-slate-950/60 p-3.5 rounded-lg border border-slate-800/80 space-y-2.5"
              >
                <div class="flex justify-between items-baseline font-mono border-b border-slate-800/60 pb-1.5">
                  <span class="font-semibold text-indigo-300">Turn #{{ turn.sequence || idx + 1 }} ({{ turn.phase || 'act' }})</span>
                  <span class="text-slate-400">{{ turn.model || 'unknown model' }} | {{ turn.duration_ms || 0 }}ms</span>
                </div>
                <div class="flex items-center space-x-3 text-slate-400 font-mono text-[11px]">
                  <span>输入: {{ turn.input_tokens || 0 }} tokens</span>
                  <span>输出: {{ turn.output_tokens || 0 }} tokens</span>
                  <span v-if="turn.cache_hit_tokens">命中缓存: {{ turn.cache_hit_tokens }} tokens</span>
                </div>

                <!-- 工具调用 -->
                <div v-if="turn.tool_calls && turn.tool_calls.length" class="space-y-1.5 pt-1">
                  <div class="text-[11px] font-semibold text-slate-400 uppercase">工具调用 ({{ turn.tool_calls.length }}):</div>
                  <div
                    v-for="(call, cIdx) in turn.tool_calls"
                    :key="cIdx"
                    class="bg-slate-900/90 p-2 rounded border border-slate-800 font-mono text-[11px] space-y-1"
                  >
                    <div class="flex justify-between text-slate-300">
                      <span class="text-amber-300 font-semibold">&gt; {{ call.tool_name }}</span>
                      <span :class="call.status === 'success' ? 'text-emerald-400' : 'text-rose-400'">{{ call.status }} ({{ call.duration_ms || 0 }}ms)</span>
                    </div>
                    <div v-if="call.arguments_summary" class="text-slate-400 truncate">
                      参数: {{ call.arguments_summary }}
                    </div>
                    <div v-if="call.error" class="text-rose-400 font-sans">
                      错误: {{ call.error }}
                    </div>
                  </div>
                </div>
              </div>

              <!-- 原始 Payload -->
              <div class="pt-2">
                <div class="font-semibold text-slate-300 uppercase tracking-wider text-xs mb-1">完整 Trace Payload:</div>
                <pre class="bg-slate-950 p-2.5 rounded border border-slate-800/80 font-mono text-xs leading-relaxed text-slate-300 whitespace-pre-wrap break-all max-h-52 overflow-y-auto no-scrollbar">{{ JSON.stringify(selectedTrace.payload, null, 2) }}</pre>
              </div>
            </div>

            <!-- 抽屉底部关闭 -->
            <div class="pt-3.5 border-t border-slate-800 flex justify-end shrink-0">
              <button
                @click="selectedTrace = null"
                class="text-xs bg-slate-800 hover:bg-slate-700 text-slate-300 font-medium px-4 py-1.5 rounded-lg transition active:scale-95"
              >
                关闭 (Esc)
              </button>
            </div>
          </div>
        </div>
      </section>
    </main>
  </div>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue'

const currentView = ref('issues')

// Issues 状态
const issues = ref([])
const projects = ref([])
const selected = ref(null)
const selectedEvents = ref([])
const filterStatus = ref('attention')
const filterPlatform = ref('')
const filterProject = ref('')
const searchKeyword = ref('')
const isRefreshing = ref(false)
const copySuccess = ref(null)

// Traces 状态
const traces = ref([])
const selectedTrace = ref(null)
const filterTraceStatus = ref('')
const traceSearchKeyword = ref('')

const traceUnreviewedCount = computed(() => traces.value.filter(t => t.eval_status === 'unreviewed').length)
const traceGoodCount = computed(() => traces.value.filter(t => t.eval_status === 'triage_good').length)
const traceBadCount = computed(() => traces.value.filter(t => t.eval_status === 'triage_bad').length)
const traceOptimizedCount = computed(() => traces.value.filter(t => t.eval_status === 'optimized').length)

const filteredTraces = computed(() => {
  let list = traces.value
  if (filterTraceStatus.value) {
    list = list.filter(t => t.eval_status === filterTraceStatus.value)
  }
  if (!traceSearchKeyword.value.trim()) return list
  const kw = traceSearchKeyword.value.trim().toLowerCase()
  return list.filter(t => {
    return (t.session_id && t.session_id.toLowerCase().includes(kw)) ||
      (t.run_id && t.run_id.toLowerCase().includes(kw)) ||
      (t.environment && t.environment.toLowerCase().includes(kw)) ||
      (t.release && t.release.toLowerCase().includes(kw))
  })
})

const getTraceTurns = (trace) => {
  if (!trace || !trace.payload) return []
  if (Array.isArray(trace.payload.turns)) return trace.payload.turns
  return []
}

const fetchTraces = async () => {
  try {
    const res = await fetch('/api/v1/traces?limit=100')
    if (res.ok) {
      traces.value = await res.json()
    }
  } catch (e) {
    console.error('加载 Trace 列表失败', e)
  }
}

const openTraceDetail = (trace) => {
  selectedTrace.value = trace
}

const updateTraceStatus = async (id, status) => {
  try {
    const res = await fetch(`/api/v1/traces/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ eval_status: status })
    })
    if (res.ok) {
      const updated = await res.json()
      if (selectedTrace.value && selectedTrace.value.id === id) {
        selectedTrace.value.eval_status = updated.eval_status
      }
      const idx = traces.value.findIndex(t => t.id === id)
      if (idx !== -1) {
        traces.value[idx].eval_status = updated.eval_status
      }
    }
  } catch (e) {
    console.error('更新 Trace 评估状态失败', e)
  }
}

const formatEvalStatusText = (status) => {
  switch (status) {
    case 'unreviewed': return '待评估'
    case 'triage_good': return '标杆样本'
    case 'triage_bad': return '缺陷样本'
    case 'eval_dataset': return '评测集'
    case 'optimized': return '已优化'
    case 'wontfix': return '已放弃'
    default: return status || '未评估'
  }
}

const evalBadgeClass = (status) => {
  switch (status) {
    case 'unreviewed':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-amber-950/80 text-amber-400 border border-amber-800/80'
    case 'triage_good':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-emerald-950/80 text-emerald-400 border border-emerald-800/80'
    case 'triage_bad':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-rose-950/80 text-rose-400 border border-rose-800/80'
    case 'eval_dataset':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-purple-950/80 text-purple-400 border border-purple-800/80'
    case 'optimized':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-indigo-950/80 text-indigo-400 border border-indigo-800/80'
    case 'wontfix':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-slate-800 text-slate-400 border border-slate-700/60'
    default:
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-slate-800 text-slate-300'
  }
}

const traceBorderClass = (status) => {
  switch (status) {
    case 'triage_good': return 'border-emerald-500'
    case 'triage_bad': return 'border-rose-500'
    case 'eval_dataset': return 'border-purple-500'
    case 'optimized': return 'border-indigo-500'
    case 'unreviewed': return 'border-amber-500'
    default: return 'border-slate-700'
  }
}

const unresolvedCount = computed(() => issues.value.filter(i => i.status === 'unresolved').length)
const inProgressCount = computed(() => issues.value.filter(i => i.status === 'in_progress').length)
const resolvedCount = computed(() => issues.value.filter(i => i.status === 'resolved').length)
const attentionCount = computed(() => issues.value.filter(i => i.status === 'unresolved' || i.status === 'in_progress' || i.status === 'regression').length)

const filteredIssues = computed(() => {
  let list = issues.value

  // 状态筛选：如果是 attention，只展示需关注的缺陷
  if (filterStatus.value === 'attention') {
    list = list.filter(i => i.status === 'unresolved' || i.status === 'in_progress' || i.status === 'regression')
  } else if (filterStatus.value) {
    list = list.filter(i => i.status === filterStatus.value)
  }

  // 平台与项目本地双重防护过滤
  if (filterPlatform.value) {
    list = list.filter(i => i.platform === filterPlatform.value)
  }
  if (filterProject.value) {
    list = list.filter(i => i.project === filterProject.value)
  }

  if (!searchKeyword.value.trim()) return list
  const kw = searchKeyword.value.trim().toLowerCase()
  return list.filter(i => {
    return (i.title && i.title.toLowerCase().includes(kw)) ||
      (i.culprit && i.culprit.toLowerCase().includes(kw)) ||
      (i.fingerprint && i.fingerprint.toLowerCase().includes(kw)) ||
      (i.project && i.project.toLowerCase().includes(kw))
  })
})

const fetchIssues = async () => {
  // 请求服务端获取全量 issue（以便前端精准统计 KPI，状态过滤由 filteredIssues 控制）
  let url = '/api/v1/issues?'
  if (filterPlatform.value) url += `platform=${filterPlatform.value}&`
  if (filterProject.value) url += `project=${encodeURIComponent(filterProject.value)}&`
  try {
    const res = await fetch(url)
    if (res.ok) {
      issues.value = await res.json()
    }
  } catch (e) {
    console.error('加载缺陷列表失败', e)
  }
}

const fetchProjects = async () => {
  try {
    const res = await fetch('/api/v1/projects')
    if (res.ok) {
      projects.value = await res.json()
    }
  } catch (e) {
    console.error('加载项目列表失败', e)
  }
}

const refreshAll = async () => {
  isRefreshing.value = true
  try {
    await Promise.all([fetchProjects(), fetchIssues(), fetchTraces()])
  } finally {
    isRefreshing.value = false
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
      const updated = await res.json()
      if (selected.value && selected.value.id === id) {
        selected.value.status = updated.status
      }
      fetchIssues()
    }
  } catch (e) {
    console.error('更新缺陷状态失败', e)
  }
}

const selectIssue = async (issue) => {
  selected.value = issue
  selectedEvents.value = []
  try {
    const res = await fetch(`/api/v1/issues/${issue.id}/events`)
    if (res.ok) {
      selectedEvents.value = await res.json()
    }
  } catch (e) {
    console.error('获取缺陷事件详情失败', e)
  }
}

const copyText = async (text, label) => {
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
    copySuccess.value = label
    setTimeout(() => {
      if (copySuccess.value === label) copySuccess.value = null
    }, 1500)
  } catch (e) {
    console.error('复制失败', e)
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
      minute: '2-digit'
    })
  } catch {
    return isoStr
  }
}

const formatStatusText = (status) => {
  switch (status) {
    case 'unresolved': return '待处理'
    case 'in_progress': return '诊断中'
    case 'resolved': return '已解决'
    case 'regression': return '再次复现'
    case 'ignored': return '已忽略'
    default: return status
  }
}

const statusBadgeClass = (status) => {
  switch (status) {
    case 'unresolved':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-rose-950/80 text-rose-400 border border-rose-800/80'
    case 'in_progress':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-amber-950/80 text-amber-400 border border-amber-800/80'
    case 'resolved':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-emerald-950/80 text-emerald-400 border border-emerald-800/80'
    case 'regression':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-purple-950/80 text-purple-400 border border-purple-800/80'
    case 'ignored':
      return 'inline-flex px-2 py-0.5 rounded text-xs font-semibold bg-slate-800 text-slate-400 border border-slate-700/60'
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

const handleKeydown = (e) => {
  if (e.key === 'Escape' && selected.value) {
    selected.value = null
  } else if (e.key === 'r' && !['INPUT', 'SELECT', 'TEXTAREA'].includes(e.target.tagName)) {
    refreshAll()
  }
}

onMounted(() => {
  refreshAll()
  window.addEventListener('keydown', handleKeydown)
})

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeydown)
})
</script>
