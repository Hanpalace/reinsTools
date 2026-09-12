<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useMessage } from 'naive-ui';
import { invoke } from '@tauri-apps/api/core';

interface ConnectionConfig {
  id: string | null;
  name: string;
  type: 'mysql' | 'postgresql' | 'sqlite';
  host: string;
  port: number | null;
  username: string;
  password: string;
  database: string;
}

const message = useMessage();

// ---------- 查询条件 ----------
const query = ref({ type: 'all', name: '', database: '' }); // 输入草稿
const applied = ref({ type: 'all', name: '', database: '' }); // 已应用条件

const typeOptions = [
  { label: '全部', value: 'all' },
  { label: 'MySQL', value: 'mysql' },
  { label: 'PostgreSQL', value: 'postgresql' },
  { label: 'SQLite', value: 'sqlite' },
];

const typeTag = (t: string): 'success' | 'info' | 'warning' =>
  t === 'mysql' ? 'success' : t === 'postgresql' ? 'info' : 'warning';

const tableColumns = [
  { key: 'name', title: '名称' },
  { key: 'type', title: '类型' },
  { key: 'host', title: '主机' },
  { key: 'port', title: '端口' },
  { key: 'database', title: '数据库' },
  { key: 'operation', title: '操作' },
];

const doQuery = () => {
  applied.value = { ...query.value };
  page.value = 1;
};

const resetQuery = () => {
  query.value = { type: 'all', name: '', database: '' };
  applied.value = { ...query.value };
  page.value = 1;
};

// ---------- 查询列表 ----------
const savedList = ref<ConnectionConfig[]>([]);

const filteredList = computed(() => {
  const t = applied.value.type;
  const name = applied.value.name.trim().toLowerCase();
  const db = applied.value.database.trim().toLowerCase();
  return savedList.value.filter((c) => {
    if (t !== 'all' && c.type !== t) return false;
    if (name && !c.name.toLowerCase().includes(name)) return false;
    if (db && !(c.database || '').toLowerCase().includes(db)) return false;
    return true;
  });
});

// ---------- 分页 ----------
const page = ref(1);
const pageSize = ref(10);

const pagedList = computed(() => {
  // 边界钳制：pageSize 变化或数据变化后页码仍可能越界，保证不出现空页
  const maxPage = Math.max(1, Math.ceil(filteredList.value.length / pageSize.value));
  const current = Math.min(page.value, maxPage);
  const start = (current - 1) * pageSize.value;
  return filteredList.value.slice(start, start + pageSize.value);
});

const loadList = async () => {
  try {
    savedList.value = await invoke<ConnectionConfig[]>('list_connections');
  } catch (e) {
    message.error(`加载连接列表失败：${toError(e)}`);
  }
};

// ---------- 新增 / 编辑弹窗 ----------
const showForm = ref(false);
const editingName = ref<string | null>(null); // null = 新增（仅用于弹窗标题）

const emptyForm = (): ConnectionConfig => ({
  id: null,
  name: '',
  type: 'mysql',
  host: '',
  port: 3306,
  username: '',
  password: '',
  database: '',
});

const form = ref<ConnectionConfig>(emptyForm());
const testing = ref(false);
const saving = ref(false);

const openAdd = () => {
  editingName.value = null;
  form.value = emptyForm();
  showForm.value = true;
};

const openEdit = (cfg: ConnectionConfig) => {
  editingName.value = cfg.name;
  form.value = { ...cfg };
  showForm.value = true;
};

// 仅用户手动切换类型时填充默认端口；程序化回填（编辑）不触发
const onTypeChange = (t: string) => {
  form.value.port = t === 'mysql' ? 3306 : t === 'postgresql' ? 5432 : 0;
};

// Tauri 对 Result<_, String> 以字符串 reject，防御性归一化
const toError = (e: unknown): string =>
  typeof e === 'string' ? e
    : e instanceof Error ? e.message
      : JSON.stringify(e);

const validate = (): string | null => {
  if (!form.value.name.trim()) return '连接名称不能为空';
  if (!form.value.host.trim()) return form.value.type === 'sqlite' ? '请填写 SQLite 文件路径' : '主机地址不能为空';
  if (form.value.type !== 'sqlite' && (form.value.port == null || form.value.port < 1 || form.value.port > 65535)) return '端口无效';
  return null;
};

const payload = (): ConnectionConfig => ({
  ...form.value,
  name: form.value.name.trim(),
  port: form.value.type === 'sqlite' ? 0 : form.value.port ?? 0,
});

const testInForm = async () => {
  const err = validate();
  if (err) { message.warning(err); return; }
  testing.value = true;
  try {
    const result = await invoke<string>('test_connection', { config: payload() });
    message.success(result);
  } catch (e) {
    message.error(`连接失败：${toError(e)}`);
  } finally {
    testing.value = false;
  }
};

const saveForm = async () => {
  const err = validate();
  if (err) { message.warning(err); return; }
  saving.value = true;
  try {
    // 后端返回更新后的完整列表，直接用，省一次 IPC + 磁盘读取
    savedList.value = await invoke<ConnectionConfig[]>('save_connection', { config: payload() });
    message.success('保存成功');
    showForm.value = false;
  } catch (e) {
    message.error(`保存失败：${toError(e)}`);
  } finally {
    saving.value = false;
  }
};

// ---------- 行操作 ----------
const testingId = ref<string | null>(null);

const testRow = async (cfg: ConnectionConfig) => {
  testingId.value = cfg.id;
  try {
    const result = await invoke<string>('test_connection', { config: cfg });
    message.success(result);
  } catch (e) {
    message.error(`连接失败：${toError(e)}`);
  } finally {
    testingId.value = null;
  }
};

const deleteRow = async (id: string | null) => {
  if (!id) return;
  try {
    // 后端返回删除后的列表，直接用
    savedList.value = await invoke<ConnectionConfig[]>('delete_connection', { id });
    message.success('删除成功');
  } catch (e) {
    message.error(`删除失败：${toError(e)}`);
  }
};

onMounted(loadList);
</script>

<template>
  <div class="space-y-6">
    <!-- 查询条件 -->
    <n-card>
      <template #header>
        <div class="flex items-center gap-2">
          <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
          <span class="font-medium">查询条件</span>
        </div>
      </template>
      <n-form inline label-placement="left" :show-feedback="false" class="flex items-center justify-center flex-wrap gap-x-5 gap-y-3">
        <n-form-item label="数据库类型">
          <n-select v-model:value="query.type" :options="typeOptions" style="width: 140px" />
        </n-form-item>
        <n-form-item label="连接名称">
          <n-input v-model:value="query.name" placeholder="名称模糊查询" clearable style="width: 180px" @keyup.enter="doQuery" />
        </n-form-item>
        <n-form-item label="数据库名">
          <n-input v-model:value="query.database" placeholder="请输入数据库名" clearable style="width: 180px" @keyup.enter="doQuery" />
        </n-form-item>
      </n-form>
      <div class="flex justify-center gap-2 mt-4">
        <n-button type="primary" @click="doQuery">查询</n-button>
        <n-button @click="resetQuery">重置</n-button>
      </div>
    </n-card>

    <!-- 工具栏：右侧新增 -->
    <div class="flex justify-end">
      <n-button type="primary" @click="openAdd">新增</n-button>
    </div>

    <!-- 查询列表 -->
    <QueryTable
      title="查询列表"
      :columns="tableColumns"
      :data="pagedList"
      row-key="id"
      v-model:page="page"
      v-model:page-size="pageSize"
      :item-count="filteredList.length"
    >
      <template #name="{ row }">
        <span class="font-medium">{{ row.name }}</span>
      </template>
      <template #type="{ row }">
        <n-tag :type="typeTag(row.type)" size="small" :bordered="false">{{ row.type }}</n-tag>
      </template>
      <template #host="{ row }">
        <span class="font-mono text-xs max-w-48 truncate inline-block align-middle" :title="row.host">{{ row.host }}</span>
      </template>
      <template #port="{ row }">
        {{ row.type === 'sqlite' ? '-' : row.port }}
      </template>
      <template #database="{ row }">
        <span class="text-gray-500">{{ row.database || '-' }}</span>
      </template>
      <template #operation="{ row }">
        <div class="flex justify-center gap-2">
          <n-button size="tiny" quaternary @click="openEdit(row)">编辑</n-button>
          <n-button size="tiny" quaternary type="primary" :loading="testingId === row.id" @click="testRow(row)">测试联通性</n-button>
          <n-popconfirm @positive-click="deleteRow(row.id)">
            <template #trigger>
              <n-button size="tiny" quaternary type="error">删除</n-button>
            </template>
            确认删除该连接配置？
          </n-popconfirm>
        </div>
      </template>
    </QueryTable>

    <!-- 新增 / 编辑弹窗 -->
    <n-modal
      v-model:show="showForm"
      preset="card"
      :title="editingName ? '编辑连接' : '新增连接'"
      style="width: 560px"
      :mask-closable="false"
    >
      <n-form :model="form" label-placement="left" label-width="100">
        <n-form-item label="连接名称">
          <n-input v-model:value="form.name" placeholder="请输入连接名称" />
        </n-form-item>
        <n-form-item label="数据库类型">
          <n-select v-model:value="form.type" :options="typeOptions.filter(o => o.value !== 'all')" @update:value="onTypeChange" />
        </n-form-item>
        <n-form-item label="主机地址">
          <n-input
            v-model:value="form.host"
            :placeholder="form.type === 'sqlite' ? 'SQLite 文件绝对路径，例如 D:\\data\\test.db' : '例如 127.0.0.1'"
          />
        </n-form-item>
        <n-form-item label="端口">
          <n-input-number v-model:value="form.port" :min="1" :max="65535" :disabled="form.type === 'sqlite'" />
        </n-form-item>
        <n-form-item label="用户名">
          <n-input v-model:value="form.username" placeholder="请输入用户名" />
        </n-form-item>
        <n-form-item label="密码">
          <n-input v-model:value="form.password" type="password" show-password-on="click" placeholder="请输入密码" />
        </n-form-item>
        <n-form-item label="数据库名">
          <n-input v-model:value="form.database" placeholder="请输入数据库名" />
        </n-form-item>
      </n-form>
      <template #footer>
        <div class="flex justify-end gap-3">
          <n-button @click="showForm = false">取消</n-button>
          <n-button :loading="testing" @click="testInForm">测试连接</n-button>
          <n-button type="primary" :loading="saving" @click="saveForm">保存</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>
