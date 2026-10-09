<template>
  <div class="min-h-screen bg-[#0c0a09] text-stone-200 flex flex-col font-sans selection:bg-stone-700 selection:text-stone-100 antialiased">
    <!-- 顶部导航栏 -->
    <header class="border-b border-stone-800/70 bg-[#0c0a09]/90 backdrop-blur-md px-5 py-3 flex items-center justify-between sticky top-0 z-40">
      <div class="flex items-center gap-6">
        <div class="flex items-center gap-2.5">
          <div class="w-7 h-7 rounded-md bg-stone-100 text-stone-900 flex items-center justify-center font-bold text-xs tracking-tighter font-mono shadow-sm">
            PS
          </div>
          <div class="flex items-baseline gap-2">
            <span class="text-sm font-semibold tracking-tight text-stone-100">PonySentry</span>
            <span class="text-[10px] font-mono text-stone-500 uppercase tracking-wider">Console</span>
          </div>
        </div>

        <!-- 视图切换 -->
        <Tabs
          v-model="currentView"
          :items="[
            { label: 'Issues', value: 'issues' },
            { label: 'Traces', value: 'traces' }
          ]"
        />
      </div>

      <div class="flex items-center gap-2.5">
        <!-- 网关状态 -->
        <span
          data-testid="gateway-status"
          :data-state="gatewayState"
          class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-[11px] font-mono border transition-colors"
          :class="gatewayBadgeClass(gatewayState)"
        >
          <span
            class="w-1.5 h-1.5 rounded-full"
            :class="[gatewayDotClass(gatewayState), { 'animate-pulse': gatewayState === 'checking' }]"
          ></span>
          {{ gatewayStateText }}
        </span>

        <Button
          variant="secondary"
          size="sm"
          @click="refreshAll"
          title="快捷键: R"
        >
          <RefreshCw class="w-3.5 h-3.5 text-stone-400" :class="{ 'animate-spin': isRefreshing }" />
          <span>刷新</span>
        </Button>
      </div>
    </header>

    <!-- 主视口 -->
    <main class="flex-1 max-w-7xl w-full mx-auto p-5 space-y-4">
      <!-- 错误横幅 -->
      <div
        v-show="showLoadError"
        data-testid="load-error"
        role="alert"
        class="flex items-center justify-between gap-3 bg-red-950/40 border border-red-900/60 text-red-200 rounded-lg px-3.5 py-2.5 text-xs shadow-sm"
      >
        <div class="flex items-center gap-2 min-w-0">
          <AlertTriangle class="w-4 h-4 text-red-400 shrink-0" />
          <span>数据加载失败</span>
        </div>
        <div class="flex items-center gap-2 shrink-0">
          <Button variant="destructive" size="sm" @click="refreshAll">重试</Button>
          <button @click="dismissLoadError" aria-label="关闭" class="text-stone-400 hover:text-stone-200 text-sm leading-none px-1">
            &times;
          </button>
        </div>
      </div>

      <!-- 视图 1：Issues -->
      <section v-if="currentView === 'issues'" class="space-y-4">
        <!-- KPI 卡片组 -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          <Card
            @click="filterStatus = 'attention'"
            @keydown.enter.prevent="filterStatus = 'attention'"
            @keydown.space.prevent="filterStatus = 'attention'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-stone-400 border-stone-600 bg-stone-900/80': filterStatus === 'attention' }"
          >
            <div class="text-[11px] font-mono uppercase text-stone-400">需关注</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-stone-100">{{ attentionCount }}</span>
              <span class="text-[11px] font-mono text-stone-500">待处理/处理中/复现</span>
            </div>
          </Card>

          <Card
            @click="filterStatus = 'unresolved'"
            @keydown.enter.prevent="filterStatus = 'unresolved'"
            @keydown.space.prevent="filterStatus = 'unresolved'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-rose-500/60 border-rose-800/80 bg-rose-950/20': filterStatus === 'unresolved' }"
          >
            <div class="text-[11px] font-mono uppercase text-rose-400">待处理</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-rose-300">{{ unresolvedCount }}</span>
              <span class="text-[11px] font-mono text-rose-400/80">Unresolved</span>
            </div>
          </Card>

          <Card
            @click="filterStatus = 'in_progress'"
            @keydown.enter.prevent="filterStatus = 'in_progress'"
            @keydown.space.prevent="filterStatus = 'in_progress'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-amber-500/60 border-amber-800/80 bg-amber-950/20': filterStatus === 'in_progress' }"
          >
            <div class="text-[11px] font-mono uppercase text-amber-400">处理中</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-amber-300">{{ inProgressCount }}</span>
              <span class="text-[11px] font-mono text-amber-400/80">In Progress</span>
            </div>
          </Card>

          <Card
            @click="filterStatus = 'resolved'"
            @keydown.enter.prevent="filterStatus = 'resolved'"
            @keydown.space.prevent="filterStatus = 'resolved'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-emerald-500/60 border-emerald-800/80 bg-emerald-950/20': filterStatus === 'resolved' }"
          >
            <div class="text-[11px] font-mono uppercase text-emerald-400">已解决</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-emerald-300">{{ resolvedCount }}</span>
              <span class="text-[11px] font-mono text-emerald-400/80">Resolved</span>
            </div>
          </Card>
        </div>

        <!-- 过滤栏 -->
        <Card class="p-2.5 flex flex-wrap items-center justify-between gap-3">
          <div class="flex flex-wrap items-center gap-2 flex-1 max-w-3xl">
            <div class="relative flex-1 min-w-[220px]">
              <Search class="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-stone-500 pointer-events-none" />
              <Input
                v-model="searchKeyword"
                placeholder="搜索标题、路径、指纹..."
                class="pl-8 pr-7"
              />
              <button
                v-if="searchKeyword"
                @click="searchKeyword = ''"
                class="absolute right-2 top-1/2 -translate-y-1/2 text-stone-400 hover:text-stone-200 text-xs"
              >&times;</button>
            </div>

            <Select v-model="filterStatus">
              <option value="attention">需关注 (默认)</option>
              <option value="unresolved">待处理</option>
              <option value="in_progress">处理中</option>
              <option value="regression">复现</option>
              <option value="resolved">已解决</option>
              <option value="ignored">已忽略</option>
              <option value="">全部状态</option>
            </Select>

            <Select v-model="filterPlatform" @change="fetchIssues">
              <option value="">全部平台</option>
              <option value="rust">Rust</option>
              <option value="tauri">Tauri</option>
              <option value="vue">Vue</option>
              <option value="python">FastAPI</option>
            </Select>

            <Select v-model="filterProject" @change="fetchIssues">
              <option value="">全部项目</option>
              <option v-for="p in projects" :key="p" :value="p">{{ p }}</option>
            </Select>
          </div>

          <div class="text-xs text-stone-400 font-mono">
            <span>匹配: </span>
            <span class="text-stone-200 font-semibold">{{ filteredIssues.length }}</span>
          </div>
        </Card>

        <!-- 数据表格 -->
        <Card class="overflow-hidden">
          <div class="overflow-x-auto">
            <table class="w-full text-left border-collapse table-fixed">
              <thead>
                <tr class="border-b border-stone-800 text-stone-400 text-[11px] font-mono uppercase bg-stone-900/60">
                  <th class="py-2.5 px-3.5 w-[42%]">Issue / Culprit</th>
                  <th class="py-2.5 px-3.5 w-[10%]">平台</th>
                  <th class="py-2.5 px-3.5 w-[12%]">项目</th>
                  <th class="py-2.5 px-3.5 w-[10%]">状态</th>
                  <th class="py-2.5 px-3.5 w-[7%] text-center">频次</th>
                  <th class="py-2.5 px-3.5 w-[11%]">指派</th>
                  <th class="py-2.5 px-3.5 w-[12%] text-right">时间</th>
                </tr>
              </thead>
              <tbody class="divide-y divide-stone-800/60 text-xs">
                <!-- 骨架屏 -->
                <template v-if="initialLoading && issues.length === 0">
                  <tr v-for="n in 5" :key="'issue-skel-' + n">
                    <td colspan="7" class="py-3 px-3.5">
                      <Skeleton class="h-3.5 w-3/5 mb-1.5" />
                      <Skeleton class="h-3 w-2/5" />
                    </td>
                  </tr>
                </template>
                <!-- 空态 -->
                <tr v-else-if="filteredIssues.length === 0">
                  <td colspan="7" class="py-12 text-center text-stone-500 font-mono">
                    {{ showLoadError ? '数据加载失败' : '无匹配记录' }}
                  </td>
                </tr>
                <!-- 数据行 -->
                <tr
                  v-for="issue in filteredIssues"
                  :key="issue.id"
                  class="hover:bg-stone-800/30 transition cursor-pointer select-none"
                  tabindex="0"
                  role="button"
                  @click="selectIssue(issue, $event)"
                  @keydown.enter.prevent="selectIssue(issue, $event)"
                  @keydown.space.prevent="selectIssue(issue, $event)"
                >
                  <td class="py-2.5 px-3.5 overflow-hidden max-w-0">
                    <div class="font-medium text-stone-200 truncate hover:text-stone-100" :title="issue.title">
                      {{ issue.title }}
                    </div>
                    <div class="text-[11px] text-stone-500 truncate mt-0.5 font-mono">
                      {{ issue.culprit || '未指定代码栈' }}
                    </div>
                  </td>
                  <td class="py-2.5 px-3.5 truncate">
                    <Badge variant="outline">{{ issue.platform }}</Badge>
                  </td>
                  <td class="py-2.5 px-3.5 truncate font-mono text-stone-300">
                    {{ issue.project || '-' }}
                  </td>
                  <td class="py-2.5 px-3.5 truncate">
                    <Badge :variant="statusBadgeVariant(issue.status)">
                      {{ formatStatusText(issue.status) }}
                    </Badge>
                  </td>
                  <td class="py-2.5 px-3.5 text-center font-mono font-medium text-stone-200">
                    {{ issue.count }}
                  </td>
                  <td class="py-2.5 px-3.5 font-mono text-[11px] truncate">
                    <span v-if="issue.assigned_to" class="text-stone-300 bg-stone-800/80 px-1.5 py-0.5 rounded border border-stone-700/60">
                      {{ issue.assigned_to }}
                    </span>
                    <span v-else class="text-stone-600">-</span>
                  </td>
                  <td class="py-2.5 px-3.5 text-right font-mono text-stone-500">
                    <span data-testid="rel-time" :title="absTimeISO(issue.last_seen_at)">{{ relTime(issue.last_seen_at) }}</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </Card>

        <!-- Issue 详情抽屉 -->
        <AnimatePresence>
          <div
            v-if="selected"
            class="fixed inset-0 bg-black/70 backdrop-blur-xs flex justify-end z-50"
            @click.self="closeIssueDrawer"
          >
            <motion.div
              :initial="{ x: '100%', opacity: 0.8 }"
              :animate="{ x: 0, opacity: 1 }"
              :exit="{ x: '100%', opacity: 0.8 }"
              :transition="{ duration: 0.18, ease: 'easeOut' }"
              role="dialog"
              aria-modal="true"
              aria-label="Issue 详情"
              tabindex="-1"
              class="w-full max-w-2xl bg-[#110f0e] border-l border-stone-800 h-full p-5 flex flex-col shadow-2xl overflow-hidden"
            >
              <!-- 头部 -->
              <div class="flex items-start justify-between border-b border-stone-800/80 pb-3.5 shrink-0">
                <div class="space-y-1.5">
                  <div class="flex items-center gap-2">
                    <Badge :variant="statusBadgeVariant(selected.status)">
                      {{ formatStatusText(selected.status) }}
                    </Badge>
                    <Badge variant="outline">{{ selected.platform }}</Badge>
                    <Badge v-if="selected.project" variant="neutral">{{ selected.project }}</Badge>
                  </div>
                  <h2 class="text-sm font-semibold text-stone-100 break-all leading-snug">{{ selected.title }}</h2>
                  <div class="flex items-center gap-2 text-[11px] font-mono text-stone-400">
                    <span>指纹: <code class="text-stone-300">{{ selected.fingerprint }}</code></span>
                    <Button variant="ghost" size="sm" class="h-5 px-1.5 text-[10px]" @click="copyText(selected.fingerprint, '指纹')">
                      <Copy class="w-2.5 h-2.5" />
                      <span>{{ copySuccess === '指纹' ? '已复制' : '复制' }}</span>
                    </Button>
                  </div>
                </div>
                <Button variant="ghost" size="icon" @click="closeIssueDrawer" title="Esc">
                  <X class="w-4 h-4" />
                </Button>
              </div>

              <!-- 状态流转操作 -->
              <div class="flex items-center justify-between bg-stone-900/40 px-3 py-2 border-b border-stone-800/80 shrink-0 text-xs">
                <span class="text-stone-400 font-mono text-[11px]">标记状态:</span>
                <div class="flex items-center gap-1.5">
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="selected.status === 'unresolved'"
                    @click="updateStatus(selected.id, 'unresolved')"
                    :class="{ 'bg-rose-950/50 border-rose-800 text-rose-300': selected.status === 'unresolved' }"
                  >待处理</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="selected.status === 'in_progress'"
                    @click="updateStatus(selected.id, 'in_progress')"
                    :class="{ 'bg-amber-950/50 border-amber-800 text-amber-300': selected.status === 'in_progress' }"
                  >处理中</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="selected.status === 'resolved'"
                    @click="updateStatus(selected.id, 'resolved')"
                    :class="{ 'bg-emerald-950/50 border-emerald-800 text-emerald-300': selected.status === 'resolved' }"
                  >已解决</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="selected.status === 'ignored'"
                    @click="updateStatus(selected.id, 'ignored')"
                    :class="{ 'bg-stone-800 border-stone-700 text-stone-300': selected.status === 'ignored' }"
                  >忽略</Button>
                </div>
              </div>

              <!-- 主内容 -->
              <div class="space-y-4 flex-1 flex flex-col min-h-0 pt-3.5">
                <!-- Culprit -->
                <div class="shrink-0 space-y-1.5">
                  <div class="flex items-center justify-between text-[11px] font-mono text-stone-400">
                    <span>出错栈 Culprit</span>
                    <button
                      v-if="selected.culprit"
                      @click="copyText(selected.culprit, 'Culprit')"
                      class="text-stone-400 hover:text-stone-200 text-[10px]"
                    >
                      {{ copySuccess === 'Culprit' ? '已复制' : '复制栈' }}
                    </button>
                  </div>
                  <pre class="font-mono text-xs leading-relaxed text-stone-300 break-all select-all bg-stone-900/60 p-2.5 rounded border border-stone-800/80 whitespace-pre-wrap">{{ selected.culprit || '未获取到栈位置' }}</pre>
                </div>

                <!-- 元数据 -->
                <div class="grid grid-cols-4 gap-2 bg-stone-900/40 p-2.5 rounded border border-stone-800/80 shrink-0 text-xs font-mono">
                  <div>
                    <span class="text-stone-500 text-[10px] block">首次发生</span>
                    <span class="text-stone-300 text-[11px]">{{ formatDate(selected.first_seen_at) }}</span>
                  </div>
                  <div>
                    <span class="text-stone-500 text-[10px] block">累计频次</span>
                    <span class="text-stone-100 font-semibold text-sm">{{ selected.count }}</span>
                  </div>
                  <div>
                    <span class="text-stone-500 text-[10px] block">版本</span>
                    <span class="text-stone-300 text-[11px] truncate block">{{ selected.last_release || '-' }}</span>
                  </div>
                  <div>
                    <span class="text-stone-500 text-[10px] block">指派</span>
                    <span class="text-stone-300 text-[11px] truncate block">{{ selected.assigned_to || '-' }}</span>
                  </div>
                </div>

                <!-- 事件列表 -->
                <div class="flex-1 flex flex-col min-h-0">
                  <div class="flex items-baseline justify-between mb-1.5 shrink-0 text-[11px] font-mono text-stone-400">
                    <span>事件记录 ({{ selectedEvents.length }})</span>
                    <span class="text-stone-500">已脱敏</span>
                  </div>
                  <div class="flex-1 overflow-y-auto no-scrollbar space-y-3 pr-0.5">
                    <p v-if="selectedEvents.length === 0" class="py-6 text-center text-xs font-mono text-stone-500">
                      {{ selectedEventsError ? '事件加载失败' : '暂无事件明细' }}
                    </p>
                    <article
                      v-for="ev in selectedEvents"
                      :key="ev.id"
                      class="bg-stone-900/50 p-3 rounded border border-stone-800/80 space-y-2 text-xs"
                    >
                      <div class="flex justify-between items-baseline font-mono text-stone-400 text-[11px] border-b border-stone-800/60 pb-1">
                        <span class="text-stone-300 font-semibold">#{{ ev.id }}</span>
                        <span>{{ formatDate(ev.created_at) }}</span>
                      </div>

                      <div v-if="ev.payload && ev.payload.breadcrumbs && ev.payload.breadcrumbs.length" class="space-y-1">
                        <span class="text-[10px] font-mono uppercase text-stone-500">Breadcrumbs:</span>
                        <div class="bg-stone-950/70 p-2 rounded border border-stone-800/60 space-y-0.5 font-mono text-[11px] max-h-28 overflow-y-auto no-scrollbar">
                          <div
                            v-for="(b, idx) in ev.payload.breadcrumbs"
                            :key="idx"
                            class="flex items-baseline gap-1.5 text-stone-300"
                          >
                            <span class="text-stone-600">{{ idx + 1 }}.</span>
                            <span class="text-stone-400">[{{ b.category || 'log' }}]</span>
                            <span class="truncate">{{ b.message || JSON.stringify(b.data || {}) }}</span>
                          </div>
                        </div>
                      </div>

                      <div>
                        <span class="text-[10px] font-mono uppercase text-stone-500">Payload:</span>
                        <pre class="bg-stone-950/70 p-2 rounded border border-stone-800/60 font-mono text-[11px] text-stone-300 whitespace-pre-wrap break-all max-h-44 overflow-y-auto no-scrollbar">{{ JSON.stringify(ev.payload, null, 2) }}</pre>
                      </div>
                    </article>
                  </div>
                </div>
              </div>

              <div class="pt-3 border-t border-stone-800/80 flex justify-end shrink-0">
                <Button variant="secondary" size="sm" @click="closeIssueDrawer">关闭</Button>
              </div>
            </motion.div>
          </div>
        </AnimatePresence>
      </section>

      <!-- 视图 2：Traces -->
      <section v-else-if="currentView === 'traces'" class="space-y-4">
        <!-- Trace KPI -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          <Card
            @click="filterTraceStatus = 'unreviewed'"
            @keydown.enter.prevent="filterTraceStatus = 'unreviewed'"
            @keydown.space.prevent="filterTraceStatus = 'unreviewed'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-amber-500/60 border-amber-800/80 bg-amber-950/20': filterTraceStatus === 'unreviewed' }"
          >
            <div class="text-[11px] font-mono uppercase text-amber-400">待评估</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-amber-300">{{ traceUnreviewedCount }}</span>
              <span class="text-[11px] font-mono text-amber-400/80">Unreviewed</span>
            </div>
          </Card>

          <Card
            @click="filterTraceStatus = 'triage_good'"
            @keydown.enter.prevent="filterTraceStatus = 'triage_good'"
            @keydown.space.prevent="filterTraceStatus = 'triage_good'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-emerald-500/60 border-emerald-800/80 bg-emerald-950/20': filterTraceStatus === 'triage_good' }"
          >
            <div class="text-[11px] font-mono uppercase text-emerald-400">标杆</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-emerald-300">{{ traceGoodCount }}</span>
              <span class="text-[11px] font-mono text-emerald-400/80">Good</span>
            </div>
          </Card>

          <Card
            @click="filterTraceStatus = 'triage_bad'"
            @keydown.enter.prevent="filterTraceStatus = 'triage_bad'"
            @keydown.space.prevent="filterTraceStatus = 'triage_bad'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-rose-500/60 border-rose-800/80 bg-rose-950/20': filterTraceStatus === 'triage_bad' }"
          >
            <div class="text-[11px] font-mono uppercase text-rose-400">缺陷</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-rose-300">{{ traceBadCount }}</span>
              <span class="text-[11px] font-mono text-rose-400/80">Bad</span>
            </div>
          </Card>

          <Card
            @click="filterTraceStatus = 'optimized'"
            @keydown.enter.prevent="filterTraceStatus = 'optimized'"
            @keydown.space.prevent="filterTraceStatus = 'optimized'"
            tabindex="0"
            role="button"
            class="p-3.5 cursor-pointer transition-all hover:border-stone-700"
            :class="{ 'ring-1 ring-stone-400 border-stone-600 bg-stone-900/80': filterTraceStatus === 'optimized' }"
          >
            <div class="text-[11px] font-mono uppercase text-stone-400">已优化</div>
            <div class="mt-2 flex items-baseline justify-between">
              <span class="text-2xl font-mono font-medium text-stone-100">{{ traceOptimizedCount }}</span>
              <span class="text-[11px] font-mono text-stone-500">Optimized</span>
            </div>
          </Card>
        </div>

        <!-- 过滤栏 -->
        <Card class="p-2.5 flex flex-wrap items-center justify-between gap-3">
          <div class="flex flex-wrap items-center gap-2 flex-1 max-w-3xl">
            <div class="relative flex-1 min-w-[220px]">
              <Search class="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-stone-500 pointer-events-none" />
              <Input
                v-model="traceSearchKeyword"
                placeholder="搜索 Session ID, Run ID, 环境..."
                class="pl-8 pr-7"
              />
              <button
                v-if="traceSearchKeyword"
                @click="traceSearchKeyword = ''"
                class="absolute right-2 top-1/2 -translate-y-1/2 text-stone-400 hover:text-stone-200 text-xs"
              >&times;</button>
            </div>

            <Select v-model="filterTraceStatus">
              <option value="">全部状态</option>
              <option value="unreviewed">待评估</option>
              <option value="triage_good">标杆样本</option>
              <option value="triage_bad">缺陷样本</option>
              <option value="eval_dataset">评测集</option>
              <option value="optimized">已优化</option>
              <option value="wontfix">已放弃</option>
            </Select>
          </div>

          <div class="text-xs text-stone-400 font-mono">
            <span>匹配: </span>
            <span class="text-stone-200 font-semibold">{{ filteredTraces.length }}</span>
          </div>
        </Card>

        <!-- Trace 列表 -->
        <Card class="overflow-hidden">
          <div v-if="initialLoading && traces.length === 0" class="divide-y divide-stone-800/60">
            <div v-for="n in 4" :key="'trace-skel-' + n" class="p-3.5">
              <Skeleton class="h-3.5 w-64 mb-1.5" />
              <Skeleton class="h-3 w-80" />
            </div>
          </div>
          <div v-else-if="filteredTraces.length === 0" class="py-12 text-center text-stone-500 font-mono text-xs">
            {{ showLoadError ? '数据加载失败' : '暂无匹配 Trace' }}
          </div>
          <div v-else class="divide-y divide-stone-800/60">
            <div
              v-for="trace in filteredTraces"
              :key="trace.id"
              @click="openTraceDetail(trace, $event)"
              @keydown.enter.prevent="openTraceDetail(trace, $event)"
              @keydown.space.prevent="openTraceDetail(trace, $event)"
              tabindex="0"
              role="button"
              class="p-3.5 hover:bg-stone-800/30 transition cursor-pointer flex flex-col md:flex-row md:items-center justify-between gap-3 select-none"
            >
              <div class="space-y-1 flex-1 min-w-0">
                <div class="flex items-center gap-2">
                  <span class="font-mono text-xs font-semibold text-stone-200 truncate">{{ trace.session_id }}</span>
                  <Badge :variant="evalBadgeVariant(trace.eval_status)">
                    {{ formatEvalStatusText(trace.eval_status) }}
                  </Badge>
                  <Badge variant="outline">{{ trace.environment }}</Badge>
                  <span class="text-[11px] font-mono text-stone-500">{{ trace.release }}</span>
                </div>
                <div class="flex items-center gap-4 text-[11px] text-stone-400 font-mono">
                  <span>Turns: <strong class="text-stone-300 font-medium">{{ getTraceTurns(trace).length }}</strong></span>
                  <span>Tokens: <strong class="text-stone-300 font-medium">{{ (trace.total_input_tokens || 0) + (trace.total_output_tokens || 0) }}</strong></span>
                  <span>耗时: <strong class="text-stone-300 font-medium">{{ trace.total_duration_ms || 0 }}ms</strong></span>
                </div>
              </div>
              <div class="text-right shrink-0 font-mono text-xs text-stone-500">
                <span data-testid="rel-time" :title="absTimeISO(trace.reported_at || trace.created_at)">{{ relTime(trace.reported_at || trace.created_at) }}</span>
              </div>
            </div>
          </div>
        </Card>

        <!-- Trace 详情抽屉 -->
        <AnimatePresence>
          <div
            v-if="selectedTrace"
            class="fixed inset-0 bg-black/70 backdrop-blur-xs flex justify-end z-50"
            @click.self="closeTraceDrawer"
          >
            <motion.div
              :initial="{ x: '100%', opacity: 0.8 }"
              :animate="{ x: 0, opacity: 1 }"
              :exit="{ x: '100%', opacity: 0.8 }"
              :transition="{ duration: 0.18, ease: 'easeOut' }"
              role="dialog"
              aria-modal="true"
              aria-label="Trace 详情"
              tabindex="-1"
              class="w-full max-w-3xl bg-[#110f0e] border-l border-stone-800 h-full p-5 flex flex-col shadow-2xl overflow-hidden"
            >
              <!-- 头部 -->
              <div class="flex items-start justify-between border-b border-stone-800/80 pb-3.5 shrink-0">
                <div class="space-y-1">
                  <div class="flex items-center gap-2">
                    <h2 class="text-sm font-semibold text-stone-100 font-mono">{{ selectedTrace.session_id }}</h2>
                    <Badge :variant="evalBadgeVariant(selectedTrace.eval_status)">
                      {{ formatEvalStatusText(selectedTrace.eval_status) }}
                    </Badge>
                  </div>
                  <p class="text-[11px] text-stone-500 font-mono">
                    ID: {{ selectedTrace.id }} · {{ formatDate(selectedTrace.reported_at) }}
                  </p>
                </div>
                <Button variant="ghost" size="icon" @click="closeTraceDrawer">
                  <X class="w-4 h-4" />
                </Button>
              </div>

              <!-- 标注栏 -->
              <div class="bg-stone-900/40 px-3 py-2 border-b border-stone-800/80 flex items-center justify-between shrink-0 text-xs">
                <span class="text-stone-400 font-mono text-[11px]">标注状态:</span>
                <div class="flex items-center gap-1.5 flex-wrap">
                  <Button
                    variant="outline"
                    size="sm"
                    @click="updateTraceStatus(selectedTrace.id, 'triage_good')"
                    :class="{ 'bg-emerald-950/50 border-emerald-800 text-emerald-300': selectedTrace.eval_status === 'triage_good' }"
                  >标杆</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    @click="updateTraceStatus(selectedTrace.id, 'triage_bad')"
                    :class="{ 'bg-rose-950/50 border-rose-800 text-rose-300': selectedTrace.eval_status === 'triage_bad' }"
                  >缺陷</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    @click="updateTraceStatus(selectedTrace.id, 'eval_dataset')"
                    :class="{ 'bg-purple-950/50 border-purple-800 text-purple-300': selectedTrace.eval_status === 'eval_dataset' }"
                  >评测集</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    @click="updateTraceStatus(selectedTrace.id, 'optimized')"
                    :class="{ 'bg-stone-800 border-stone-700 text-stone-200': selectedTrace.eval_status === 'optimized' }"
                  >已优化</Button>
                  <Button
                    variant="outline"
                    size="sm"
                    @click="updateTraceStatus(selectedTrace.id, 'wontfix')"
                    :class="{ 'bg-stone-900 border-stone-800 text-stone-400': selectedTrace.eval_status === 'wontfix' }"
                  >已放弃</Button>
                </div>
              </div>

              <!-- 明细 -->
              <div class="flex-1 overflow-y-auto space-y-3 pt-3.5 pr-0.5 no-scrollbar text-xs">
                <div class="text-[11px] font-mono uppercase text-stone-400">执行轨迹 Turns</div>
                <div v-if="getTraceTurns(selectedTrace).length === 0" class="text-stone-500 py-6 text-center font-mono">
                  无 turns 明细
                </div>
                <div
                  v-for="(turn, idx) in getTraceTurns(selectedTrace)"
                  :key="idx"
                  class="bg-stone-900/50 p-3 rounded border border-stone-800/80 space-y-2"
                >
                  <div class="flex justify-between items-baseline font-mono text-[11px] border-b border-stone-800/60 pb-1">
                    <span class="font-semibold text-stone-300">Turn #{{ turn.sequence || idx + 1 }} ({{ turn.phase || 'act' }})</span>
                    <span class="text-stone-500">{{ turn.model || '-' }} · {{ turn.duration_ms || 0 }}ms</span>
                  </div>
                  <div class="flex items-center gap-3 text-stone-400 font-mono text-[11px]">
                    <span>输入: {{ turn.input_tokens || 0 }}</span>
                    <span>输出: {{ turn.output_tokens || 0 }}</span>
                    <span v-if="turn.cache_hit_tokens">缓存命中: {{ turn.cache_hit_tokens }}</span>
                  </div>

                  <div v-if="turn.tool_calls && turn.tool_calls.length" class="space-y-1 pt-1">
                    <div class="text-[10px] font-mono uppercase text-stone-500">工具调用:</div>
                    <div
                      v-for="(call, cIdx) in turn.tool_calls"
                      :key="cIdx"
                      class="bg-stone-950/70 p-2 rounded border border-stone-800/60 font-mono text-[11px] space-y-0.5"
                    >
                      <div class="flex justify-between text-stone-300">
                        <span class="font-medium text-stone-200">&gt; {{ call.tool_name }}</span>
                        <span :class="call.status === 'success' ? 'text-emerald-400' : 'text-rose-400'">{{ call.status }} ({{ call.duration_ms || 0 }}ms)</span>
                      </div>
                      <div v-if="call.arguments_summary" class="text-stone-500 truncate">
                        参数: {{ call.arguments_summary }}
                      </div>
                      <div v-if="call.error" class="text-rose-400">
                        错误: {{ call.error }}
                      </div>
                    </div>
                  </div>
                </div>

                <div class="pt-2">
                  <div class="text-[11px] font-mono uppercase text-stone-400 mb-1">Payload 详情</div>
                  <pre class="bg-stone-950/70 p-2.5 rounded border border-stone-800/80 font-mono text-[11px] text-stone-300 whitespace-pre-wrap break-all max-h-48 overflow-y-auto no-scrollbar">{{ JSON.stringify(selectedTrace.payload, null, 2) }}</pre>
                </div>
              </div>

              <div class="pt-3 border-t border-stone-800/80 flex justify-end shrink-0">
                <Button variant="secondary" size="sm" @click="closeTraceDrawer">关闭</Button>
              </div>
            </motion.div>
          </div>
        </AnimatePresence>
      </section>
    </main>
  </div>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import { motion, AnimatePresence } from 'motion-v'
import { RefreshCw, Search, X, Copy, AlertTriangle } from '@lucide/vue'

import Button from '@/components/ui/Button.vue'
import Badge from '@/components/ui/Badge.vue'
import Card from '@/components/ui/Card.vue'
import Input from '@/components/ui/Input.vue'
import Select from '@/components/ui/Select.vue'
import Tabs from '@/components/ui/Tabs.vue'
import Skeleton from '@/components/ui/Skeleton.vue'

const API_TIMEOUT_MS = 3000

const currentView = ref('issues')

const issues = ref([])
const projects = ref([])
const selected = ref(null)
const selectedEvents = ref([])
const selectedEventsError = ref(false)
let currentIssueEventsController = null
let currentIssueEventsReqId = 0
const filterStatus = ref('attention')
const filterPlatform = ref('')
const filterProject = ref('')
const searchKeyword = ref('')
const isRefreshing = ref(false)
const copySuccess = ref(null)

const initialLoading = ref(true)
const issueLoadFailed = ref(false)
const traceLoadFailed = ref(false)
const projectLoadFailed = ref(false)
const showLoadError = computed(() => issueLoadFailed.value || traceLoadFailed.value || projectLoadFailed.value)
const dismissLoadError = () => {
  issueLoadFailed.value = false
  traceLoadFailed.value = false
  projectLoadFailed.value = false
}

const gatewayState = ref('checking')
const gatewayStateText = computed(() => {
  if (gatewayState.value === 'ready') return '就绪'
  if (gatewayState.value === 'degraded') return '异常'
  return '探测中'
})
const gatewayBadgeClass = (state) => {
  switch (state) {
    case 'ready':
      return 'bg-emerald-950/40 text-emerald-400 border-emerald-900/50'
    case 'degraded':
      return 'bg-rose-950/40 text-rose-400 border-rose-900/50'
    default:
      return 'bg-stone-900/60 text-stone-400 border-stone-800'
  }
}
const gatewayDotClass = (state) => {
  switch (state) {
    case 'ready': return 'bg-emerald-400'
    case 'degraded': return 'bg-rose-400'
    default: return 'bg-stone-400'
  }
}

const nowRef = ref(Date.now())
let relTimeTimer = null

const relTime = (isoStr) => {
  if (!isoStr) return '-'
  const d = new Date(isoStr)
  if (Number.isNaN(d.getTime())) return '-'
  const diffSec = Math.max(0, Math.floor((nowRef.value - d.getTime()) / 1000))
  if (diffSec < 60) return '0分钟前'
  const min = Math.floor(diffSec / 60)
  if (min < 1440) return `${min}分钟前`
  return `${Math.floor(min / 1440)}天前`
}

const absTimeISO = (isoStr) => {
  if (!isoStr) return ''
  const d = new Date(isoStr)
  return Number.isNaN(d.getTime()) ? isoStr : d.toISOString()
}

const allControllers = new Set()
const refreshControllers = new Set()

async function apiFetchJson(url, options = {}) {
  const { timeoutMs = API_TIMEOUT_MS, refreshScope = false, signal: externalSignal, ...fetchOptions } = options
  const controller = new AbortController()
  let timedOut = false
  allControllers.add(controller)
  if (refreshScope) refreshControllers.add(controller)

  let onExternalAbort = null
  if (externalSignal) {
    if (externalSignal.aborted) {
      controller.abort()
    } else {
      onExternalAbort = () => controller.abort()
      externalSignal.addEventListener('abort', onExternalAbort, { once: true })
    }
  }

  const timeoutId = setTimeout(() => {
    timedOut = true
    controller.abort()
  }, timeoutMs)
  try {
    const response = await fetch(url, { ...fetchOptions, signal: controller.signal })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    return await response.json()
  } catch (e) {
    if (e && e.name === 'AbortError') {
      if (timedOut) {
        const err = new Error('请求超时')
        err.name = 'TimeoutError'
        throw err
      }
      const err = new Error('请求已取消')
      err.name = 'AbortError'
      throw err
    }
    throw e
  } finally {
    clearTimeout(timeoutId)
    if (externalSignal && onExternalAbort) {
      externalSignal.removeEventListener('abort', onExternalAbort)
    }
    allControllers.delete(controller)
    refreshControllers.delete(controller)
  }
}

function abortRefreshFetches() {
  refreshControllers.forEach((c) => c.abort())
  refreshControllers.clear()
}

function abortAllControllers() {
  allControllers.forEach((c) => c.abort())
  allControllers.clear()
  refreshControllers.clear()
}

let healthTimer = null
let healthController = null
let checkingDebounceTimer = null

async function checkHealth() {
  if (checkingDebounceTimer) {
    clearTimeout(checkingDebounceTimer)
    checkingDebounceTimer = null
  }
  checkingDebounceTimer = setTimeout(() => {
    gatewayState.value = 'checking'
  }, 250)

  if (healthController) healthController.abort()
  healthController = new AbortController()
  const timeoutId = setTimeout(() => healthController.abort(), API_TIMEOUT_MS)
  try {
    const res = await fetch('/healthz', { signal: healthController.signal })
    gatewayState.value = res.ok ? 'ready' : 'degraded'
  } catch {
    gatewayState.value = 'degraded'
  } finally {
    clearTimeout(timeoutId)
    if (checkingDebounceTimer) {
      clearTimeout(checkingDebounceTimer)
      checkingDebounceTimer = null
    }
    healthController = null
  }
}

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
    traces.value = await apiFetchJson('/api/v1/traces?limit=100', { refreshScope: true })
    traceLoadFailed.value = false
  } catch (e) {
    if (e.name === 'AbortError') return
    console.error('加载 Trace 列表失败', e)
    traceLoadFailed.value = true
  }
}

const openTraceDetail = (trace, event) => {
  traceTriggerEl = event?.currentTarget || null
  selectedTrace.value = trace
  focusDialogPanel()
}

const updateTraceStatus = async (id, status) => {
  try {
    const updated = await apiFetchJson(`/api/v1/traces/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ eval_status: status })
    })
    if (selectedTrace.value && selectedTrace.value.id === id) {
      selectedTrace.value.eval_status = updated.eval_status
    }
    const idx = traces.value.findIndex(t => t.id === id)
    if (idx !== -1) {
      traces.value[idx].eval_status = updated.eval_status
    }
  } catch (e) {
    if (e.name === 'AbortError') return
    console.error('更新 Trace 评估状态失败', e)
  }
}

const formatEvalStatusText = (status) => {
  switch (status) {
    case 'unreviewed': return '待评估'
    case 'triage_good': return '标杆'
    case 'triage_bad': return '缺陷'
    case 'eval_dataset': return '评测集'
    case 'optimized': return '已优化'
    case 'wontfix': return '已放弃'
    default: return status || '未评估'
  }
}

const evalBadgeVariant = (status) => {
  switch (status) {
    case 'unreviewed': return 'warning'
    case 'triage_good': return 'success'
    case 'triage_bad': return 'danger'
    case 'eval_dataset': return 'neutral'
    case 'optimized': return 'default'
    case 'wontfix': return 'outline'
    default: return 'outline'
  }
}

const unresolvedCount = computed(() => issues.value.filter(i => i.status === 'unresolved').length)
const inProgressCount = computed(() => issues.value.filter(i => i.status === 'in_progress').length)
const resolvedCount = computed(() => issues.value.filter(i => i.status === 'resolved').length)
const attentionCount = computed(() => issues.value.filter(i => i.status === 'unresolved' || i.status === 'in_progress' || i.status === 'regression').length)

const filteredIssues = computed(() => {
  let list = issues.value

  if (filterStatus.value === 'attention') {
    list = list.filter(i => i.status === 'unresolved' || i.status === 'in_progress' || i.status === 'regression')
  } else if (filterStatus.value) {
    list = list.filter(i => i.status === filterStatus.value)
  }

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
  let url = '/api/v1/issues?'
  if (filterPlatform.value) url += `platform=${filterPlatform.value}&`
  if (filterProject.value) url += `project=${encodeURIComponent(filterProject.value)}&`
  try {
    issues.value = await apiFetchJson(url, { refreshScope: true })
    issueLoadFailed.value = false
  } catch (e) {
    if (e.name === 'AbortError') return
    console.error('加载缺陷列表失败', e)
    issueLoadFailed.value = true
  }
}

const fetchProjects = async () => {
  try {
    projects.value = await apiFetchJson('/api/v1/projects', { refreshScope: true })
    projectLoadFailed.value = false
  } catch (e) {
    if (e.name === 'AbortError') return
    console.error('加载项目列表失败', e)
    projectLoadFailed.value = true
  }
}

const refreshAll = async () => {
  isRefreshing.value = true
  abortRefreshFetches()
  try {
    await Promise.all([fetchProjects(), fetchIssues(), fetchTraces()])
  } catch (e) {
    console.error('刷新失败', e)
  } finally {
    isRefreshing.value = false
    initialLoading.value = false
  }
}

const updateStatus = async (id, status) => {
  try {
    const updated = await apiFetchJson(`/api/v1/issues/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status })
    })
    if (selected.value && selected.value.id === id) {
      selected.value.status = updated.status
    }
    fetchIssues()
  } catch (e) {
    if (e.name === 'AbortError') return
    console.error('更新缺陷状态失败', e)
  }
}

const selectIssue = async (issue, event) => {
  issueTriggerEl = event?.currentTarget || null
  selected.value = issue
  selectedEvents.value = []
  selectedEventsError.value = false
  focusDialogPanel()

  if (currentIssueEventsController) {
    currentIssueEventsController.abort()
  }
  const controller = new AbortController()
  currentIssueEventsController = controller
  const reqId = ++currentIssueEventsReqId

  try {
    const data = await apiFetchJson(`/api/v1/issues/${issue.id}/events`, {
      signal: controller.signal
    })
    if (currentIssueEventsReqId === reqId && selected.value?.id === issue.id) {
      selectedEvents.value = data
    }
  } catch (e) {
    if (e.name === 'AbortError') return
    if (currentIssueEventsReqId === reqId && selected.value?.id === issue.id) {
      console.error('获取缺陷事件详情失败', e)
      selectedEventsError.value = true
    }
  } finally {
    if (currentIssueEventsController === controller) {
      currentIssueEventsController = null
    }
  }
}

let issueTriggerEl = null
let traceTriggerEl = null

function focusDialogPanel() {
  nextTick(() => {
    const panel = document.querySelector('[role="dialog"][aria-modal="true"]')
    if (panel && !panel.contains(document.activeElement)) panel.focus()
  })
}

function restoreFocus() {
  nextTick(() => {
    if (issueTriggerEl && document.contains(issueTriggerEl)) {
      issueTriggerEl.focus()
    }
    issueTriggerEl = null
    if (traceTriggerEl && document.contains(traceTriggerEl)) {
      traceTriggerEl.focus()
    }
    traceTriggerEl = null
  })
}

const closeIssueDrawer = () => {
  if (currentIssueEventsController) {
    currentIssueEventsController.abort()
    currentIssueEventsController = null
  }
  currentIssueEventsReqId++
  selected.value = null
  selectedEvents.value = []
  selectedEventsError.value = false
  restoreFocus()
}

const closeTraceDrawer = () => {
  selectedTrace.value = null
  restoreFocus()
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
    case 'in_progress': return '处理中'
    case 'resolved': return '已解决'
    case 'regression': return '复现'
    case 'ignored': return '已忽略'
    default: return status
  }
}

const statusBadgeVariant = (status) => {
  switch (status) {
    case 'unresolved': return 'danger'
    case 'in_progress': return 'warning'
    case 'resolved': return 'success'
    case 'regression': return 'neutral'
    case 'ignored': return 'outline'
    default: return 'outline'
  }
}

const handleKeydown = (e) => {
  if (e.key === 'Escape') {
    if (selected.value) {
      closeIssueDrawer()
    } else if (selectedTrace.value) {
      closeTraceDrawer()
    }
    return
  }
  if (e.key === 'Tab') {
    const panel = document.querySelector('[role="dialog"][aria-modal="true"]')
    if (!panel) return
    const focusables = Array.from(
      panel.querySelectorAll('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])')
    )
    if (!focusables.length) return
    const first = focusables[0]
    const last = focusables[focusables.length - 1]
    const outside = !panel.contains(document.activeElement)
    if (e.shiftKey && (document.activeElement === first || outside)) {
      e.preventDefault()
      last.focus()
    } else if (!e.shiftKey && (document.activeElement === last || outside)) {
      e.preventDefault()
      first.focus()
    }
    return
  }
  if (e.key === 'r' && !['INPUT', 'SELECT', 'TEXTAREA'].includes(e.target.tagName)) {
    refreshAll()
  }
}

onMounted(() => {
  checkHealth()
  healthTimer = setInterval(checkHealth, 15000)
  relTimeTimer = setInterval(() => { nowRef.value = Date.now() }, 30000)
  refreshAll()
  window.addEventListener('keydown', handleKeydown)
})

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeydown)
  if (healthTimer) clearInterval(healthTimer)
  if (relTimeTimer) clearInterval(relTimeTimer)
  if (checkingDebounceTimer) {
    clearTimeout(checkingDebounceTimer)
    checkingDebounceTimer = null
  }
  if (healthController) healthController.abort()
  if (currentIssueEventsController) {
    currentIssueEventsController.abort()
    currentIssueEventsController = null
  }
  abortAllControllers()
})
</script>
