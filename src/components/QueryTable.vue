<script setup lang="ts">
// 公用查询列表表格：统一的「查询列表」样式
// - 卡片容器 + 右上角「共 N 条」计数
// - 表头统一居中，单元格默认居中（长文本列建议 align: 'left'）
// - 斑马纹 + 行悬浮高亮（支持深色模式）、单行模式、空态、右下角分页
// 用法：<QueryTable title="查询列表" :columns="columns" :data="rows" row-key="id"
//          v-model:page="page" v-model:page-size="pageSize" :item-count="total">
//         <template #某列key="{ row }">自定义单元格内容</template>
//         <template #detail="{ row }">可选：每行下方的明细行（跨全列）</template>
//       </QueryTable>
import { computed, useSlots } from 'vue'

const slots = useSlots()
// 提供 #detail 插槽时，每行下方渲染一条跨全列的明细行
const hasDetail = computed(() => !!slots.detail)

const emit = defineEmits<{
  'update:page': [value: number]
  'update:pageSize': [value: number]
}>()

defineProps<{
  title: string
  columns: { key: string; title: string; align?: 'left' | 'center' | 'right' }[]
  data: Record<string, any>[]
  rowKey: string
  page: number
  pageSize: number
  itemCount: number
}>()
</script>

<template>
  <n-card>
    <template #header>
      <div class="flex items-center gap-2">
        <span class="inline-block w-1 h-4 rounded-full bg-teal-600"></span>
        <span class="font-medium">{{ title }}</span>
      </div>
    </template>
    <n-empty v-if="data.length === 0" description="暂无数据" />
    <n-table v-else :bordered="false" size="small" single-line>
      <thead>
        <tr>
          <th v-for="col in columns" :key="col.key" style="text-align: center">{{ col.title }}</th>
        </tr>
      </thead>
      <tbody>
        <template v-for="(row, i) in data" :key="row[rowKey]">
          <tr
            class="transition-colors hover:bg-gray-50 dark:hover:bg-gray-800/40"
            :class="i % 2 === 1 ? 'bg-gray-50/60 dark:bg-gray-800/20' : ''"
          >
            <td
              v-for="col in columns"
              :key="col.key"
              :style="{ textAlign: col.align || 'center' }"
            >
              <slot :name="col.key" :row="row">
                {{ row[col.key] }}
              </slot>
            </td>
          </tr>
          <tr v-if="hasDetail" class="bg-gray-50/60 dark:bg-gray-800/20">
            <td :colspan="columns.length" class="px-4 py-2">
              <slot name="detail" :row="row" />
            </td>
          </tr>
        </template>
      </tbody>
    </n-table>
    <div v-if="itemCount > 0" class="flex items-center justify-between mt-4">
      <span class="text-xs text-gray-400">共 {{ itemCount }} 条</span>
      <n-pagination
        :page="page"
        :page-size="pageSize"
        :item-count="itemCount"
        :page-sizes="[10, 20, 50]"
        show-size-picker
        @update:page="(v: number) => emit('update:page', v)"
        @update:page-size="(v: number) => { emit('update:pageSize', v); emit('update:page', 1); }"
      />
    </div>
  </n-card>
</template>
