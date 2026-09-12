<script setup lang="ts">
import { ref, computed, onMounted, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import { useMessage } from 'naive-ui';
import { invoke } from '@tauri-apps/api/core';

interface TableCount { table: string; rows: number }
interface HistoryFileRecord { sha256: string; name: string; rows: number; tables: TableCount[] }
interface ReplayConfig {
  connectionId: string | null;
  filePaths: string[];
  deleteScript: { path: string | null; text: string | null } | null;
  batchRows: number;
  maxBytes: number;
  useTls: boolean;
  disableFkChecks: boolean;
  stripAutoIncrement: boolean;
}
interface HistoryRun {
  runId: number; atMs: number; ok: boolean; rows: number; remark: string;
  files: HistoryFileRecord[]; replayConfig: ReplayConfig | null;
}

const message = useMessage();
const router = useRouter();

// 跳转到批量导入页并携带重放记录 id（该页自动回填并预检）
const replayRun = (run: HistoryRun) => {
  if (!run.replayConfig) return;
  router.push({ path: '/data/batch-import', query: { replayRunId: String(run.runId) } });
};

const toError = (e: unknown): string =>
  typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e);

const formatTime = (ms: number): string => {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
};

// ---------- 运行日志 ----------
const logText = ref('');
const logLoading = ref(false);
const logBox = ref<HTMLElement | null>(null);

const refreshLog = async () => {
  logLoading.value = true;
  try {
    logText.value = await invoke<string>('read_import_log', { tailLines: 500 });
    await nextTick();
    logBox.value?.scrollTo({ top: logBox.value.scrollHeight });
  } catch (e) {
    message.error(`读取日志失败：${toError(e)}`);
  } finally {
    logLoading.value = false;
  }
};

const clearLog = async () => {
  try {
    await invoke('clear_import_log');
    logText.value = '';
    message.success('日志已清空');
  } catch (e) {
    message.error(`清空日志失败：${toError(e)}`);
  }
};

// ---------- 导入历史（增删改查） ----------
const historyRuns = ref<HistoryRun[]>([]);
const historyLoading = ref(false);

const query = ref({
  ok: 'all' as string,
  keyword: '',
  range: null as [number, number] | null,
});

const okOptions = [
  { label: '全部', value: 'all' },
  { label: '成功', value: 'success' },
  { label: '失败', value: 'error' },
];

const refreshHistory = async () => {
  historyLoading.value = true;
  try {
    historyRuns.value = await invoke<HistoryRun[]>('list_import_history', {
      keyword: query.value.keyword.trim() || null,
      ok: query.value.ok === 'all' ? null : query.value.ok === 'success' ? 1 : 0,
      fromMs: query.value.range ? query.value.range[0] : null,
      toMs: query.value.range ? query.value.range[1] : null,
    });
  } catch (e) {
    message.error(`读取导入历史失败：${toError(e)}`);
  } finally {
    historyLoading.value = false;
  }
};

const resetQuery = () => {
  query.value = { ok: 'all', keyword: '', range: null };
  refreshHistory();
};

// 列表展示（QueryTable + 明细行）
const historyPage = ref(1);
const historyPageSize = ref(10);

const historyColumns = [
  { key: 'time', title: '时间' },
  { key: 'result', title: '结果' },
  { key: 'fileCount', title: '文件数' },
  { key: 'rows', title: '总行数' },
  { key: 'remark', title: '说明', align: 'left' as const },
  { key: 'operation', title: '操作' },
];

const historyRows = computed(() =>
  historyRuns.value.map((r) => ({
    runId: r.runId,
    atMs: r.atMs,
    ok: r.ok,
    rows: r.rows,
    remark: r.remark,
    files: r.files,
    replayConfig: r.replayConfig,
    time: formatTime(r.atMs),
    fileCount: r.files.length,
  }))
);

const pagedHistory = computed(() => {
  const maxPage = Math.max(1, Math.ceil(historyRows.value.length / historyPageSize.value));
  const current = Math.min(historyPage.value, maxPage);
  const start = (current - 1) * historyPageSize.value;
  return historyRows.value.slice(start, start + historyPageSize.value);
});

// 登记 / 编辑弹窗
const showEdit = ref(false);
const editMode = ref<'add' | 'edit'>('add');
const editRunId = ref<number | null>(null);
const savingRun = ref(false);
const editForm = ref({
  remark: '',
  atMs: Date.now(),
  files: [{ name: '', rows: 0 }] as { name: string; rows: number }[],
});

const openAdd = () => {
  editMode.value = 'add';
  editRunId.value = null;
  editForm.value = { remark: '', atMs: Date.now(), files: [{ name: '', rows: 0 }] };
  showEdit.value = true;
};

const openEdit = (run: HistoryRun) => {
  editMode.value = 'edit';
  editRunId.value = run.runId;
  editForm.value = {
    remark: run.remark || '',
    atMs: run.atMs,
    files: run.files.map((f) => ({ name: f.name, rows: f.rows })),
  };
  showEdit.value = true;
};

const addFileRow = () => {
  editForm.value.files.push({ name: '', rows: 0 });
};

const removeFileRow = (i: number) => {
  editForm.value.files.splice(i, 1);
};

const saveRun = async () => {
  const files = editForm.value.files.filter((f) => f.name.trim());
  if (files.length === 0) {
    message.warning('请至少登记一个文件');
    return;
  }
  savingRun.value = true;
  try {
    const request = {
      remark: editForm.value.remark.trim(),
      atMs: editForm.value.atMs,
      files: files.map((f) => ({ name: f.name.trim(), rows: f.rows || 0 })),
    };
    if (editMode.value === 'add') {
      await invoke('add_history_run', { request });
      message.success('登记成功');
    } else {
      await invoke('update_history_run', { runId: editRunId.value, request });
      message.success('保存成功');
    }
    showEdit.value = false;
    await refreshHistory();
  } catch (e) {
    message.error(toError(e));
  } finally {
    savingRun.value = false;
  }
};

const deleteRun = async (runId: number) => {
  try {
    await invoke('delete_history_run', { runId });
    message.success('删除成功');
    await refreshHistory();
  } catch (e) {
    message.error(`删除失败：${toError(e)}`);
  }
};

onMounted(() => {
  refreshLog();
  refreshHistory();
});
</script>

<template>
  <div class="space-y-6">
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">导入日志</span>
        </div>
      </template>
      <n-tabs type="line" default-value="log" animated>
        <!-- 运行日志 -->
        <n-tab-pane name="log" tab="运行日志">
          <div class="flex justify-end gap-2 mb-3">
            <n-button size="small" @click="clearLog">清空</n-button>
            <n-button size="small" :loading="logLoading" @click="refreshLog">刷新</n-button>
          </div>
          <div
            ref="logBox"
            class="bg-gray-50 dark:bg-gray-800 rounded p-3 text-xs font-mono whitespace-pre-wrap leading-5 min-h-64 max-h-[55vh] overflow-auto"
          >
            {{ logText || '暂无日志记录' }}
          </div>
        </n-tab-pane>

        <!-- 导入历史 -->
        <n-tab-pane name="history" tab="导入历史">
          <!-- 查询条件 -->
          <n-form inline label-placement="left" :show-feedback="false" class="flex items-center justify-center flex-wrap gap-x-5 gap-y-3 mb-4">
            <n-form-item label="结果">
              <n-select v-model:value="query.ok" :options="okOptions" style="width: 110px" />
            </n-form-item>
            <n-form-item label="关键字">
              <n-input v-model:value="query.keyword" placeholder="文件/说明模糊查询" clearable style="width: 180px" @keyup.enter="refreshHistory" />
            </n-form-item>
            <n-form-item label="时间范围">
              <n-date-picker v-model:value="query.range" type="datetimerange" clearable style="width: 330px" />
            </n-form-item>
            <div class="flex gap-2">
              <n-button type="primary" size="small" @click="refreshHistory">查询</n-button>
              <n-button size="small" @click="resetQuery">重置</n-button>
            </div>
          </n-form>

          <!-- 工具栏 -->
          <div class="flex justify-end mb-3">
            <n-button type="primary" size="small" @click="openAdd">手工登记</n-button>
          </div>

          <!-- 列表 -->
          <QueryTable
            title="导入记录"
            :columns="historyColumns"
            :data="pagedHistory"
            row-key="runId"
            v-model:page="historyPage"
            v-model:page-size="historyPageSize"
            :item-count="historyRows.length"
          >
            <template #time="{ row }">
              <span class="font-medium">{{ row.time }}</span>
            </template>
            <template #result="{ row }">
              <n-tag :type="row.ok ? 'success' : 'error'" size="small" :bordered="false">
                {{ row.ok ? '成功' : '失败' }}
              </n-tag>
            </template>
            <template #remark="{ row }">
              <span class="text-gray-500 max-w-64 truncate inline-block align-middle" :title="row.remark">
                {{ row.remark || '-' }}
              </span>
            </template>
            <template #operation="{ row }">
              <div class="flex justify-center gap-2">
                <n-tooltip :disabled="!!row.replayConfig" trigger="hover">
                  <template #trigger>
                    <n-button size="tiny" quaternary type="primary" :disabled="!row.replayConfig" @click="replayRun(row)">
                      重新执行
                    </n-button>
                  </template>
                  旧版本台账缺少执行配置，无法重放
                </n-tooltip>
                <n-button size="tiny" quaternary @click="openEdit(row)">编辑</n-button>
                <n-popconfirm @positive-click="deleteRun(row.runId)">
                  <template #trigger>
                    <n-button size="tiny" quaternary type="error">删除</n-button>
                  </template>
                  确认删除这条导入记录？
                </n-popconfirm>
              </div>
            </template>
            <template #detail="{ row }">
              <ul class="text-xs font-mono space-y-1">
                <li v-for="(f, i) in row.files" :key="i">
                  {{ f.name || f.sha256.slice(0, 12) }}（{{ f.rows }} 行，
                  表：{{ f.tables.map((t: TableCount) => `${t.table}×${t.rows}`).join('、') || '-' }}）
                </li>
              </ul>
            </template>
          </QueryTable>
        </n-tab-pane>
      </n-tabs>
    </n-card>

    <!-- 登记 / 编辑弹窗 -->
    <n-modal
      v-model:show="showEdit"
      preset="card"
      :title="editMode === 'add' ? '手工登记导入记录' : '编辑导入记录'"
      style="width: 620px"
      :mask-closable="false"
    >
      <n-form :model="editForm" label-placement="left" label-width="90">
        <n-form-item label="导入时间">
          <n-date-picker v-model:value="editForm.atMs" type="datetime" style="width: 220px" />
        </n-form-item>
        <n-form-item label="说明">
          <n-input v-model:value="editForm.remark" type="textarea" :autosize="{ minRows: 2, maxRows: 4 }" placeholder="例如：内网环境执行 part1~part6" />
        </n-form-item>
        <n-form-item label="文件明细">
          <div class="space-y-2 w-full">
            <div v-for="(f, i) in editForm.files" :key="i" class="flex items-center gap-2">
              <n-input v-model:value="f.name" placeholder="文件名" style="width: 300px" />
              <n-input-number v-model:value="f.rows" :min="0" placeholder="行数" style="width: 110px" />
              <n-button size="tiny" quaternary type="error" :disabled="editForm.files.length === 1" @click="removeFileRow(i)">移除</n-button>
            </div>
            <n-button size="tiny" dashed @click="addFileRow">+ 添加文件</n-button>
          </div>
        </n-form-item>
      </n-form>
      <template #footer>
        <div class="flex justify-end gap-3">
          <n-button @click="showEdit = false">取消</n-button>
          <n-button type="primary" :loading="savingRun" @click="saveRun">保存</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>
