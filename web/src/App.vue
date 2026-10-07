<template>
  <div class="min-h-screen bg-slate-950 text-slate-100 flex flex-col font-sans">
    <!-- Header -->
    <header class="border-b border-slate-800 bg-slate-900/60 backdrop-blur px-6 py-4 flex items-center justify-between">
      <div class="flex items-center space-x-3">
        <div class="w-8 h-8 rounded bg-gradient-to-tr from-indigo-500 to-violet-500 flex items-center justify-center font-bold text-white shadow-lg">
          PS
        </div>
        <div>
          <h1 class="text-lg font-semibold tracking-wide">PonySentry</h1>
          <p class="text-xs text-slate-400">Lightweight Client Crash &amp; Agent Defect Diagnostics</p>
        </div>
      </div>
      <div class="flex items-center space-x-4">
        <span class="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-medium bg-emerald-950 text-emerald-400 border border-emerald-800">
          <span class="w-1.5 h-1.5 rounded-full bg-emerald-400 mr-1.5 animate-pulse"></span>
          Ingest Active
        </span>
      </div>
    </header>

    <!-- Main Content -->
    <main class="flex-1 max-w-7xl w-full mx-auto p-6 space-y-6">
      <!-- Filter Bar -->
      <div class="flex flex-wrap items-center justify-between gap-4 bg-slate-900/40 p-4 rounded-xl border border-slate-800/80">
        <div class="flex flex-wrap items-center gap-3">
          <select v-model="filterStatus" @change="fetchIssues" class="bg-slate-800 border border-slate-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500">
            <option value="">All Statuses</option>
            <option value="unresolved">Unresolved</option>
            <option value="in_progress">In Progress</option>
            <option value="resolved">Resolved</option>
            <option value="regression">Regression</option>
            <option value="ignored">Ignored</option>
          </select>

          <select v-model="filterPlatform" @change="fetchIssues" class="bg-slate-800 border border-slate-700 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500">
            <option value="">All Platforms</option>
            <option value="rust">Rust</option>
            <option value="tauri">Tauri</option>
            <option value="vue">Vue</option>
            <option value="python">FastAPI</option>
          </select>

          <button @click="fetchIssues" class="bg-slate-800 hover:bg-slate-700 text-slate-200 px-3 py-1.5 rounded-lg text-sm transition">
            Refresh
          </button>
        </div>
        <div class="text-xs text-slate-400">
          Total Issues: {{ issues.length }}
        </div>
      </div>

      <!-- Issues Table -->
      <div class="bg-slate-900/60 rounded-xl border border-slate-800/80 overflow-hidden shadow-xl">
        <table class="w-full text-left border-collapse">
          <thead>
            <tr class="border-b border-slate-800 text-slate-400 text-xs uppercase bg-slate-900/90">
              <th class="py-3 px-4">Issue / Title</th>
              <th class="py-3 px-4">Platform</th>
              <th class="py-3 px-4">Status</th>
              <th class="py-3 px-4">Count</th>
              <th class="py-3 px-4">Assignee</th>
              <th class="py-3 px-4">Last Seen</th>
              <th class="py-3 px-4 text-right">Actions</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-slate-800/60 text-sm">
            <tr v-if="issues.length === 0">
              <td colspan="7" class="py-8 text-center text-slate-500">
                No issues found. Send events to <code>/api/v1/ingest</code> to capture errors.
              </td>
            </tr>
            <tr v-for="issue in issues" :key="issue.id" class="hover:bg-slate-800/40 transition">
              <td class="py-3 px-4">
                <div class="font-medium text-slate-200 hover:text-indigo-400 cursor-pointer" @click="selectIssue(issue)">
                  {{ issue.title }}
                </div>
                <div class="text-xs text-slate-400 truncate max-w-md">
                  {{ issue.culprit || 'Unknown origin' }}
                </div>
              </td>
              <td class="py-3 px-4">
                <span class="inline-flex px-2 py-0.5 rounded text-xs font-mono bg-slate-800 text-slate-300">
                  {{ issue.platform }}
                </span>
              </td>
              <td class="py-3 px-4">
                <span :class="statusBadgeClass(issue.status)">
                  {{ issue.status }}
                </span>
              </td>
              <td class="py-3 px-4 font-mono text-slate-300">
                {{ issue.count }}
              </td>
              <td class="py-3 px-4 text-xs text-slate-400">
                {{ issue.assigned_to || '-' }}
              </td>
              <td class="py-3 px-4 text-xs text-slate-400">
                {{ formatDate(issue.last_seen_at) }}
              </td>
              <td class="py-3 px-4 text-right space-x-2">
                <button
                  v-if="issue.status !== 'resolved'"
                  @click="updateStatus(issue.id, 'resolved')"
                  class="text-xs bg-emerald-950/80 hover:bg-emerald-900 text-emerald-400 border border-emerald-800 px-2.5 py-1 rounded transition"
                >
                  Resolve
                </button>
                <button
                  v-if="issue.status === 'unresolved'"
                  @click="updateStatus(issue.id, 'in_progress')"
                  class="text-xs bg-indigo-950/80 hover:bg-indigo-900 text-indigo-400 border border-indigo-800 px-2.5 py-1 rounded transition"
                >
                  Claim
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- Detail Modal / Drawer -->
      <div v-if="selected" class="fixed inset-0 bg-black/60 backdrop-blur-sm flex justify-end z-50">
        <div class="w-full max-w-2xl bg-slate-900 border-l border-slate-800 h-full p-6 overflow-y-auto space-y-6">
          <div class="flex items-center justify-between border-b border-slate-800 pb-4">
            <div>
              <h2 class="text-lg font-semibold text-slate-100">{{ selected.title }}</h2>
              <p class="text-xs text-slate-400">Fingerprint: <code>{{ selected.fingerprint }}</code></p>
            </div>
            <button @click="selected = null" class="text-slate-400 hover:text-slate-200 text-xl font-bold">
              &times;
            </button>
          </div>

          <div class="space-y-4">
            <div>
              <h3 class="text-xs font-semibold uppercase text-slate-400 mb-1">Culprit</h3>
              <p class="bg-slate-950 p-2.5 rounded font-mono text-xs text-rose-300 border border-slate-800">
                {{ selected.culprit || 'None' }}
              </p>
            </div>

            <div class="grid grid-cols-2 gap-4">
              <div>
                <span class="text-xs text-slate-400">First Seen:</span>
                <p class="text-sm font-mono text-slate-300">{{ formatDate(selected.first_seen_at) }}</p>
              </div>
              <div>
                <span class="text-xs text-slate-400">Latest Release:</span>
                <p class="text-sm font-mono text-slate-300">{{ selected.last_release || 'N/A' }}</p>
              </div>
            </div>

            <div>
              <h3 class="text-xs font-semibold uppercase text-slate-400 mb-2">Recent Events</h3>
              <div class="space-y-2">
                <div v-for="ev in selectedEvents" :key="ev.id" class="bg-slate-950 p-3 rounded border border-slate-800 text-xs space-y-1">
                  <div class="flex justify-between text-slate-400">
                    <span>Event ID: {{ ev.id }}</span>
                    <span>{{ formatDate(ev.created_at) }}</span>
                  </div>
                  <pre class="overflow-x-auto text-slate-300 max-h-48">{{ JSON.stringify(ev.payload, null, 2) }}</pre>
                </div>
              </div>
            </div>
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
    console.error('Failed to load issues', e)
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
    console.error('Failed to update status', e)
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
    console.error('Failed to fetch events', e)
  }
}

const formatDate = (isoStr) => {
  if (!isoStr) return '-'
  try {
    return new Date(isoStr).toLocaleString()
  } catch {
    return isoStr
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

onMounted(() => {
  fetchIssues()
})
</script>
