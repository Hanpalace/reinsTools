import { createRouter, createWebHistory } from 'vue-router'
import HomeView from '../views/HomeView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: HomeView
    },
    {
      path: '/data/batch-import',
      name: 'data-batch-import',
      component: () => import('../views/DataBatchImport.vue')
    },
    {
      path: '/data/connection',
      name: 'data-connection',
      component: () => import('../views/DataConnection.vue')
    },
    {
      path: '/data/import-log',
      name: 'data-import-log',
      component: () => import('../views/ImportLog.vue')
    }
  ]
})

export default router