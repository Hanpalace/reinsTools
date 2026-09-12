<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRouter, useRoute } from 'vue-router';
import { useMessage } from 'naive-ui';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';

interface ConnectionConfig {
  id: string | null;
  name: string;
  type: string;
  host: string;
  port: number | null;
  username: string;
  password: string;
  database: string;
}

interface FileStat { path: string; name: string; size: number }
interface TableCount { table: string; rows: number }

interface ScriptInput { path?: string | null; text?: string | null }

interface ImportRequest {
  connection: ConnectionConfig;
  files: string[];
  deleteScript?: ScriptInput | null;
  batchRows: number;
  maxBytes: number;
  useTls: boolean;
  disableFkChecks: boolean;
  stripAutoIncrement: boolean;
  maxPartBytes: number;
  skipHistoryWarning: boolean;
}

interface ParsedFile {
  path: string; name: string; size: number; encoding: string; sha256: string;
  rows: number; badLines: number; badLineSamples: string[]; tableCounts: TableCount[]; batchCount: number;
}
interface DeleteImpact { no: number; sqlPreview: string; rows: number }
interface DeletePreview { statementCount: number; perStatement: DeleteImpact[]; totalRows: number; allZero: boolean }
interface HistoryEntry { sha256: string; ok: boolean; atMs: number; rows: number; tables: TableCount[]; name?: string }
interface PreviewReport {
  files: ParsedFile[]; totalRows: number; deletePreview: DeletePreview | null;
  historyHits: HistoryEntry[]; tlsWarning: string | null;
  maxAllowedPacket: number; effectiveBatchBytes: number; strippedColumns: string[];
}
interface ImportProgress { stage: string; percent: number; message: string }
interface ExportResult {
  parts: { path: string; size: number }[];
  batches: number; rows: number;
  strippedColumns: string[]; warnings: string[];
}

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
interface HistoryRunForReplay {
  runId: number; atMs: number; ok: boolean; rows: number; remark: string;
  files: unknown[]; replayConfig: ReplayConfig | null;
}
interface TableCompare { table: string; expected: number; actual: number; ok: boolean }
interface FailedBatch { batchNo: number; firstLine: number; lastLine: number; message: string }
interface DeleteStepResult { skipped: boolean; skipReason: string | null; totalRows: number; perStatement: DeleteImpact[] }
interface InsertStepResult { batches: number; rows: number; tables: TableCompare[]; failedBatch: FailedBatch | null }
interface ImportReport {
  committed: boolean; error: string | null; files: ParsedFile[];
  deleteStep: DeleteStepResult | null; insertStep: InsertStepResult;
  warnings: string[]; strippedColumns: string[];
  historyRecorded: boolean; durationMs: number;
}

const message = useMessage();
const router = useRouter();
const route = useRoute();

// ---------- 目标连接 ----------
const connections = ref<ConnectionConfig[]>([]);
const selectedConnectionId = ref<string | null>(null);

const mysqlConnections = computed(() => connections.value.filter((c) => c.type === 'mysql'));

const selectedConnection = computed(() =>
  connections.value.find((c) => c.id === selectedConnectionId.value) ?? null
);

const loadConnections = async () => {
  try {
    connections.value = await invoke<ConnectionConfig[]>('list_connections');
  } catch (e) {
    message.error(`加载连接列表失败：${toError(e)}`);
  }
};

// ---------- 导入文件 ----------
const importFiles = ref<FileStat[]>([]);

const pickImportFiles = async () => {
  const selected = await open({
    multiple: true,
    title: '选择导入文件',
    filters: [{ name: 'SQL 文本', extensions: ['txt', 'sql'] }],
  });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  const existing = new Set(importFiles.value.map((f) => f.path));
  const fresh = paths.filter((p) => !existing.has(p));
  if (fresh.length === 0) return;
  try {
    const stats = await invoke<FileStat[]>('stat_import_files', { paths: fresh });
    importFiles.value = [...importFiles.value, ...stats];
  } catch (e) {
    message.error(`读取文件信息失败：${toError(e)}`);
  }
};

const removeImportFile = (path: string) => {
  importFiles.value = importFiles.value.filter((f) => f.path !== path);
};

// ---------- 脚本输入（删除/修正/核对）----------
const scriptState = (path: string | null, text: string) => ({ path, text });

const deleteScript = ref(scriptState(null, ''));

const pickScriptFile = async () => {
  const selected = await open({
    multiple: false,
    title: '选择脚本文件',
    filters: [{ name: 'SQL 文本', extensions: ['txt', 'sql'] }],
  });
  if (typeof selected === 'string') {
    deleteScript.value.path = selected;
  }
};

const toScriptInput = (s: { path: string | null; text: string }): ScriptInput | null => {
  const text = s.text.trim();
  if (!text && !s.path) return null;
  return { path: s.path, text: text || null };
};

// ---------- 选项 ----------
const batchRows = ref(1000);
const maxBytesMb = ref(1);
const useTls = ref(false);
const disableFkChecks = ref(false);
const stripAutoIncrement = ref(true);
const exportPartMb = ref(5); // 导出分片上限；0 = 不分片

// ---------- 执行状态 ----------
const previewing = ref(false);
const executing = ref(false);
const exporting = ref(false);
const progress = ref<ImportProgress | null>(null);

const stageText: Record<string, string> = {
  connect: '连接数据库',
  parse: '解析文件',
  dry_run: '统计删除影响',
  delete: '执行删除',
  insert: '批量插入',
  commit: '提交',
  rollback: '回滚',
  done: '完成',
};

const previewReport = ref<PreviewReport | null>(null);
const showConfirm = ref(false);

const report = ref<ImportReport | null>(null);
const showReport = ref(false);

const exportResult = ref<ExportResult | null>(null);
const showExportReport = ref(false);

let unlisten: UnlistenFn | null = null;

const toError = (e: unknown): string =>
  typeof e === 'string' ? e : e instanceof Error ? e.message : JSON.stringify(e);

const formatSize = (bytes: number): string => {
  if (bytes <= 0) return '-';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  let v = bytes;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
};

const formatTime = (ms: number): string => {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
};

const buildRequest = (): ImportRequest => ({
  // 导出场景允许不选连接（空连接 → 后端跳过自增剔除）
  connection: selectedConnection.value ?? {
    id: null, name: '', type: 'mysql', host: '', port: 3306, username: '', password: '', database: '',
  },
  files: importFiles.value.map((f) => f.path),
  deleteScript: toScriptInput(deleteScript.value),
  batchRows: batchRows.value,
  maxBytes: Math.round(maxBytesMb.value * 1024 * 1024),
  useTls: useTls.value,
  disableFkChecks: disableFkChecks.value,
  stripAutoIncrement: stripAutoIncrement.value,
  maxPartBytes: Math.round(exportPartMb.value * 1024 * 1024),
  skipHistoryWarning: false,
});

// ---------- 流程 ----------
const startPreview = async () => {
  if (!selectedConnection.value) {
    message.warning('请先选择目标连接');
    return;
  }
  if (importFiles.value.length === 0) {
    message.warning('请先选择导入文件');
    return;
  }
  previewing.value = true;
  progress.value = { stage: 'parse', percent: 0, message: '预检中…' };
  try {
    previewReport.value = await invoke<PreviewReport>('preview_import', { request: buildRequest() });
    showConfirm.value = true;
  } catch (e) {
    message.error(`预检失败：${toError(e)}`);
  } finally {
    previewing.value = false;
  }
};

const confirmExecute = async () => {
  showConfirm.value = false;
  executing.value = true;
  progress.value = { stage: 'begin', percent: 0, message: '准备执行…' };
  try {
    const request = buildRequest();
    request.skipHistoryWarning = true;
    // 删除影响为 0：跳过删除步骤（不传删除脚本）
    if (previewReport.value?.deletePreview?.allZero) {
      request.deleteScript = null;
    }
    report.value = await invoke<ImportReport>('execute_import', { request });
    showReport.value = true;
  } catch (e) {
    message.error(`导入失败：${toError(e)}`);
  } finally {
    executing.value = false;
  }
};

// ---------- 导出批量脚本（离线环境用） ----------
const exportScript = async () => {
  if (importFiles.value.length === 0) {
    message.warning('请先选择导入文件');
    return;
  }
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  const defaultName = `批量导入_${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}.sql`;
  const outPath = await save({
    title: '保存批量导入脚本',
    defaultPath: defaultName,
    filters: [{ name: 'SQL 文件', extensions: ['sql'] }],
  });
  if (!outPath) return;
  exporting.value = true;
  try {
    // 未选连接时 buildRequest 返回空连接，后端跳过自增剔除、原样导出
    const result = await invoke<ExportResult>('export_import_script', { request: buildRequest(), outPath });
    // 结果统一在弹窗中按模块展示，不弹多个 toast
    exportResult.value = result;
    showExportReport.value = true;
  } catch (e) {
    message.error(`导出失败：${toError(e)}`);
  } finally {
    exporting.value = false;
  }
};

const closeReport = () => {
  showReport.value = false;
  if (report.value?.committed) {
    // 导入成功后重置页面（保留连接与选项，便于下次导入）
    importFiles.value = [];
    deleteScript.value = scriptState(null, '');
    previewReport.value = null;
  }
  // 无论成败，关闭报告后清除进度条；失败回滚时保留文件与脚本以便重试
  progress.value = null;
};

// ---------- 表格列 ----------
const fileColumns = [
  { key: 'name', title: '文件名' },
  { key: 'size', title: '大小' },
  { key: 'operation', title: '操作' },
];

const filePage = ref(1);
const filePageSize = ref(10);
const pagedFiles = computed(() => {
  const maxPage = Math.max(1, Math.ceil(importFiles.value.length / filePageSize.value));
  const current = Math.min(filePage.value, maxPage);
  const start = (current - 1) * filePageSize.value;
  return importFiles.value.slice(start, start + filePageSize.value);
});

// 重放历史记录：回填配置并自动预检（从导入历史页跳转携带 replayRunId）
const applyReplay = async (runId: number) => {
  try {
    const run = await invoke<HistoryRunForReplay | null>('get_history_run', { runId });
    if (!run?.replayConfig) {
      message.warning('该记录缺少可重放的执行配置');
      return;
    }
    const cfg = run.replayConfig;
    if (cfg.connectionId) {
      selectedConnectionId.value = cfg.connectionId;
    }
    if (cfg.filePaths.length > 0) {
      try {
        importFiles.value = await invoke<FileStat[]>('stat_import_files', { paths: cfg.filePaths });
      } catch (e) {
        message.warning(`部分导入文件不可用：${toError(e)}`);
      }
    }
    deleteScript.value = scriptState(cfg.deleteScript?.path ?? null, cfg.deleteScript?.text ?? '');
    batchRows.value = cfg.batchRows || 1000;
    maxBytesMb.value = Math.max(1, Math.round((cfg.maxBytes || 1048576) / 1048576));
    useTls.value = !!cfg.useTls;
    disableFkChecks.value = !!cfg.disableFkChecks;
    stripAutoIncrement.value = cfg.stripAutoIncrement !== false;
    if (!selectedConnection.value) {
      message.warning('该记录关联的连接已不存在，请选择目标连接后点击「开始导入」');
      return;
    }
    message.info('已回填导入配置，正在预检…');
    await startPreview();
  } catch (e) {
    message.error(`重放配置加载失败：${toError(e)}`);
  }
};

onMounted(async () => {
  await loadConnections();
  unlisten = await listen<ImportProgress>('import-progress', (e) => {
    progress.value = e.payload;
  });
  const replayId = route.query.replayRunId;
  if (replayId) {
    router.replace({ query: {} }); // 清除参数，避免刷新页面重复触发
    await applyReplay(Number(replayId));
  }
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <div class="space-y-6">
    <!-- 目标连接 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">目标连接</span>
        </div>
      </template>
      <div class="flex items-center gap-3 flex-wrap">
        <n-select
          v-model:value="selectedConnectionId"
          :options="mysqlConnections.map(c => ({ label: `${c.name}（${c.host}:${c.port}）`, value: c.id }))"
          placeholder="选择 MySQL 连接"
          style="width: 320px"
        />
        <n-button size="small" @click="loadConnections">刷新</n-button>
        <n-button size="small" quaternary type="primary" @click="router.push('/data/connection')">去配置连接 →</n-button>
      </div>
    </n-card>

    <!-- 导入文件 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">导入文件</span>
        </div>
      </template>
      <div class="flex justify-end mb-4">
        <n-button type="primary" @click="pickImportFiles">选择文件</n-button>
      </div>
      <QueryTable
        title="已选文件"
        :columns="fileColumns"
        :data="pagedFiles"
        row-key="path"
        v-model:page="filePage"
        v-model:page-size="filePageSize"
        :item-count="importFiles.length"
      >
        <template #name="{ row }">
          <span class="font-medium">{{ row.name }}</span>
        </template>
        <template #size="{ row }">
          {{ formatSize(row.size) }}
        </template>
        <template #operation="{ row }">
          <div class="flex justify-center gap-2">
            <n-popconfirm @positive-click="removeImportFile(row.path)">
              <template #trigger>
                <n-button size="tiny" quaternary type="error">移除</n-button>
              </template>
              确认移除该文件？
            </n-popconfirm>
          </div>
        </template>
      </QueryTable>
    </n-card>

    <!-- 删除脚本 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">删除脚本（可选）</span>
        </div>
      </template>
      <div class="space-y-3">
        <div class="flex items-center gap-3">
          <n-button size="small" @click="pickScriptFile">选择文件</n-button>
          <span v-if="deleteScript.path" class="text-xs text-gray-400">{{ deleteScript.path }}</span>
        </div>
        <n-input
          v-model:value="deleteScript.text"
          type="textarea"
          :autosize="{ minRows: 3, maxRows: 8 }"
          placeholder="或粘贴删除脚本（每行一条 DELETE 语句）；粘贴内容优先于所选文件"
        />
      </div>
    </n-card>

    <!-- 选项 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">选项</span>
        </div>
      </template>
      <div class="flex items-center gap-8 flex-wrap">
        <div class="flex items-center gap-2">
          <span class="text-sm text-gray-500">每批行数</span>
          <n-input-number v-model:value="batchRows" :min="100" :max="10000" style="width: 130px" />
        </div>
        <div class="flex items-center gap-2">
          <span class="text-sm text-gray-500">单批最大字节(MB)</span>
          <n-input-number v-model:value="maxBytesMb" :min="1" :max="64" style="width: 130px" />
        </div>
        <div class="flex items-center gap-2">
          <span class="text-sm text-gray-500">强制 TLS 加密</span>
          <n-switch v-model:value="useTls" />
        </div>
        <div class="flex items-center gap-2">
          <n-tooltip trigger="hover">
            <template #trigger>
              <span class="text-sm text-gray-500">临时关闭外键检查（速度优先）</span>
            </template>
            导入期间 SET FOREIGN_KEY_CHECKS=0（仅本次连接生效）；唯一约束始终开启，重复键仍会失败回滚。
          </n-tooltip>
          <n-switch v-model:value="disableFkChecks" />
        </div>
        <div class="flex items-center gap-2">
          <n-tooltip trigger="hover">
            <template #trigger>
              <span class="text-sm text-gray-500">剔除自增主键列</span>
            </template>
            自动识别目标表的自增主键列并从脚本中剔除，由目标库重新生成主键，避免与库中现有数据主键冲突。前提：各表之间通过业务键（如 acc_no/pay_acc_no）关联，而非自增主键关联。
          </n-tooltip>
          <n-switch v-model:value="stripAutoIncrement" />
        </div>
        <div class="flex items-center gap-2">
          <n-tooltip trigger="hover">
            <template #trigger>
              <span class="text-sm text-gray-500">导出分片上限(MB)</span>
            </template>
            导出批量脚本时按此上限切分为多个文件（每个分片为独立事务，按顺序执行）；0 = 不分片。内网环境有单文件大小限制时使用。
          </n-tooltip>
          <n-input-number v-model:value="exportPartMb" :min="0" :max="1024" style="width: 130px" />
        </div>
      </div>
    </n-card>

    <!-- 执行区 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">执行</span>
        </div>
      </template>
      <div class="flex justify-center gap-3 mb-4">
        <n-button type="primary" size="large" :loading="previewing || executing" :disabled="executing || exporting" @click="startPreview">
          开始导入
        </n-button>
        <n-button size="large" :loading="exporting" :disabled="previewing || executing" @click="exportScript">
          导出批量脚本
        </n-button>
      </div>
      <div v-if="progress" class="max-w-2xl mx-auto">
        <n-progress
          type="line"
          :percentage="progress.percent"
          indicator-placement="outside"
          :status="progress.stage === 'rollback' ? 'error' : progress.stage === 'done' ? 'success' : 'default'"
        />
        <p class="text-center text-sm text-gray-500 mt-2">
          {{ stageText[progress.stage] || progress.stage }} — {{ progress.message }}
        </p>
      </div>
    </n-card>

    <!-- 删除影响确认弹窗 -->
    <n-modal v-model:show="showConfirm" preset="card" title="执行确认" style="width: 720px" :mask-closable="false">
      <div v-if="previewReport" class="space-y-5 max-h-[60vh] overflow-auto pr-2">
        <!-- 连接安全 -->
        <div v-if="previewReport.tlsWarning">
          <p class="font-medium mb-2">连接安全</p>
          <n-alert type="warning" :bordered="false">
            {{ previewReport.tlsWarning }}
          </n-alert>
        </div>

        <!-- 自增主键处理 -->
        <div v-if="previewReport.strippedColumns.length > 0">
          <p class="font-medium mb-2">自增主键处理</p>
          <n-alert type="info" :bordered="false">
            将剔除脚本中的自增主键列（由目标库重新生成）：{{ previewReport.strippedColumns.join('、') }}
          </n-alert>
        </div>

        <!-- 历史导入记录 -->
        <div v-if="previewReport.historyHits.length > 0">
          <p class="font-medium mb-2">历史导入记录</p>
          <n-collapse>
            <n-collapse-item :title="`发现 ${previewReport.historyHits.length} 个文件此前已成功导入`" name="history">
              <ul class="text-xs text-amber-600 space-y-1">
                <li v-for="(h, i) in previewReport.historyHits" :key="i">
                  文件 {{ h.name || h.sha256.slice(0, 12) }}（{{ formatTime(h.atMs) }}，{{ h.rows }} 行）——重复导入确认后继续
                </li>
              </ul>
            </n-collapse-item>
          </n-collapse>
        </div>

        <!-- 删除影响 -->
        <div v-if="previewReport.deletePreview">
          <p class="font-medium mb-2">删除影响</p>
          <n-alert v-if="previewReport.deletePreview.allZero" type="info" :bordered="false">
            删除脚本影响行数为 0，将自动跳过删除步骤。
          </n-alert>
          <template v-else>
            <p class="text-sm text-gray-500 mb-2">删除脚本将影响 {{ previewReport.deletePreview.totalRows }} 行：</p>
            <n-table :bordered="false" size="small" single-line>
              <thead>
                <tr>
                  <th style="text-align: center">序号</th>
                  <th>语句</th>
                  <th style="text-align: center">影响行数</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="d in previewReport.deletePreview.perStatement" :key="d.no" class="hover:bg-gray-50">
                  <td style="text-align: center">{{ d.no }}</td>
                  <td class="font-mono text-xs max-w-96 truncate" :title="d.sqlPreview">{{ d.sqlPreview }}</td>
                  <td style="text-align: center">{{ d.rows }}</td>
                </tr>
              </tbody>
            </n-table>
          </template>
        </div>

        <!-- 汇总 -->
        <div class="text-sm text-gray-500 border-t border-gray-100 pt-3">
          共 {{ previewReport.files.length }} 个文件、{{ previewReport.totalRows }} 行数据；
          服务器 max_allowed_packet={{ formatSize(previewReport.maxAllowedPacket) }}，
          单批预算 {{ formatSize(previewReport.effectiveBatchBytes) }}。
        </div>
      </div>
      <template #footer>
        <div class="flex justify-end gap-3">
          <n-button @click="showConfirm = false">取消</n-button>
          <n-button type="primary" @click="confirmExecute">确认执行</n-button>
        </div>
      </template>
    </n-modal>

    <!-- 结果报告弹窗 -->
    <n-modal v-model:show="showReport" preset="card" title="导入结果报告" style="width: 860px" :mask-closable="false">
      <div v-if="report" class="space-y-4 max-h-[60vh] overflow-auto">
        <div class="flex items-center gap-3 flex-wrap">
          <n-tag :type="report.committed ? 'success' : 'error'" size="large">
            {{ report.committed ? '已提交' : '已回滚' }}
          </n-tag>
          <span class="text-sm text-gray-500">耗时 {{ (report.durationMs / 1000).toFixed(1) }} 秒</span>
          <span v-if="report.historyRecorded" class="text-sm text-gray-500">已记入导入台账</span>
          <span v-if="report.strippedColumns.length > 0" class="text-sm text-teal-600">
            已剔除自增主键列：{{ report.strippedColumns.join('、') }}
          </span>
        </div>

        <n-alert v-if="report.error" type="error" :bordered="false">
          {{ report.error }}
          <span v-if="report.insertStep.failedBatch" class="block mt-1">
            失败位置：第 {{ report.insertStep.failedBatch.batchNo }} 批（源文件第
            {{ report.insertStep.failedBatch.firstLine }}-{{ report.insertStep.failedBatch.lastLine }} 行）
          </span>
        </n-alert>

        <!-- 执行提示（TLS/外键等，条数少） -->
        <div v-if="report.warnings.length > 0">
          <p class="font-medium mb-2">执行提示</p>
          <n-alert v-for="(w, i) in report.warnings" :key="i" type="warning" :bordered="false" class="mb-2">{{ w }}</n-alert>
        </div>


        <div>
          <p class="font-medium mb-2">文件明细</p>
          <n-table :bordered="false" size="small" single-line>
            <thead>
              <tr>
                <th>文件</th>
                <th style="text-align: center">编码</th>
                <th style="text-align: center">行数</th>
                <th style="text-align: center">坏行</th>
                <th style="text-align: center">批次数</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="f in report.files" :key="f.path" class="hover:bg-gray-50">
                <td class="max-w-56 truncate" :title="f.path">{{ f.name }}</td>
                <td style="text-align: center">{{ f.encoding }}</td>
                <td style="text-align: center">{{ f.rows }}</td>
                <td style="text-align: center">
                  <n-tag v-if="f.badLines > 0" type="warning" size="small" :bordered="false">{{ f.badLines }}</n-tag>
                  <span v-else>0</span>
                </td>
                <td style="text-align: center">{{ f.batchCount }}</td>
              </tr>
            </tbody>
          </n-table>
        </div>

        <div v-if="report.deleteStep">
          <p class="font-medium mb-2">删除步骤</p>
          <p class="text-sm text-gray-500">
            共删除 {{ report.deleteStep.totalRows }} 行（{{ report.deleteStep.perStatement.length }} 条语句）
          </p>
        </div>

        <div>
          <p class="font-medium mb-2">行数核对（{{ report.insertStep.tables.every(t => t.ok) ? '全部通过 ✓' : '存在不符 ✗' }}）</p>
          <n-table :bordered="false" size="small" single-line>
            <thead>
              <tr>
                <th>表</th>
                <th style="text-align: center">预期</th>
                <th style="text-align: center">实际</th>
                <th style="text-align: center">结果</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="t in report.insertStep.tables" :key="t.table" class="hover:bg-gray-50">
                <td class="font-mono text-xs">{{ t.table }}</td>
                <td style="text-align: center">{{ t.expected }}</td>
                <td style="text-align: center">{{ t.actual }}</td>
                <td style="text-align: center">
                  <n-tag :type="t.ok ? 'success' : 'error'" size="small" :bordered="false">{{ t.ok ? '✓' : '✗' }}</n-tag>
                </td>
              </tr>
            </tbody>
          </n-table>
        </div>

      </div>
      <template #footer>
        <div class="flex justify-end">
          <n-button type="primary" @click="closeReport">关闭</n-button>
        </div>
      </template>
    </n-modal>

    <!-- 导出结果弹窗 -->
    <n-modal v-model:show="showExportReport" preset="card" title="导出结果" style="width: 720px" :mask-closable="false">
      <div v-if="exportResult" class="space-y-5 max-h-[60vh] overflow-auto pr-2">
        <div class="flex items-center gap-3">
          <n-tag type="success" size="large">导出完成</n-tag>
          <span class="text-sm text-gray-500">{{ exportResult.batches }} 批、{{ exportResult.rows }} 行</span>
        </div>

        <!-- 分片文件 -->
        <div>
          <p class="font-medium mb-2">分片文件（{{ exportResult.parts.length }} 个）</p>
          <n-collapse v-if="exportResult.parts.length > 3">
            <n-collapse-item :title="`共 ${exportResult.parts.length} 个文件，点击展开`" name="parts">
              <ul class="text-xs font-mono space-y-1">
                <li v-for="(p, i) in exportResult.parts" :key="p.path">
                  第 {{ i + 1 }} 片：{{ p.path }}（{{ formatSize(p.size) }}）
                </li>
              </ul>
            </n-collapse-item>
          </n-collapse>
          <ul v-else class="text-xs font-mono space-y-1">
            <li v-for="(p, i) in exportResult.parts" :key="p.path">
              第 {{ i + 1 }} 片：{{ p.path }}（{{ formatSize(p.size) }}）
            </li>
          </ul>
        </div>

        <!-- 自增主键处理 -->
        <div v-if="exportResult.strippedColumns.length > 0">
          <p class="font-medium mb-2">自增主键处理</p>
          <n-alert type="info" :bordered="false">
            已剔除自增主键列：{{ exportResult.strippedColumns.join('、') }}
          </n-alert>
        </div>

        <!-- 提示 -->
        <div v-if="exportResult.warnings.length > 0">
          <p class="font-medium mb-2">提示</p>
          <n-collapse v-if="exportResult.warnings.length > 2">
            <n-collapse-item :title="`共 ${exportResult.warnings.length} 条提示，点击展开`" name="export-warnings">
              <ul class="text-xs text-amber-600 space-y-1">
                <li v-for="(w, i) in exportResult.warnings" :key="i">{{ w }}</li>
              </ul>
            </n-collapse-item>
          </n-collapse>
          <template v-else>
            <n-alert v-for="(w, i) in exportResult.warnings" :key="i" type="warning" :bordered="false" class="mb-2">{{ w }}</n-alert>
          </template>
        </div>

        <div v-if="exportResult.parts.length > 1" class="text-sm text-gray-500 border-t border-gray-100 pt-3">
          请在内网环境按分片顺序（1→N）依次执行；每个分片为独立事务。
        </div>
      </div>
      <template #footer>
        <div class="flex justify-end">
          <n-button type="primary" @click="showExportReport = false">关闭</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>
