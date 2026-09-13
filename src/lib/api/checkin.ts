import { invoke } from "@tauri-apps/api/core";
import type {
  CheckinBrowserSessionStatus,
  CheckinConfig,
  CheckinResult,
  CheckinSite,
} from "@/types/checkin";

/**
 * 公益站签到 API。
 *
 * 每个站的签到接口形状不同，所以后端不做预设面板类型，而是执行用户
 * 完整描述的一条 HTTP 请求。凭证明文存在 SQLite 的 settings 表里，
 * 与仓库既有的 S3 secret_access_key 一致 —— 面板需向用户明示。
 */
export const checkinApi = {
  /** 打开当前条目的独立登录窗口；不会等待用户完成登录。 */
  async openLogin(id: string): Promise<void> {
    await invoke("open_checkin_login_window", { id });
  },

  /** 只读取 Cookie 数量及窗口状态，不传输账号 Cookie。 */
  async getBrowserSessionStatus(
    id: string,
  ): Promise<CheckinBrowserSessionStatus> {
    return await invoke("get_checkin_browser_session_status", { id });
  },

  async getConfig(): Promise<CheckinConfig> {
    return await invoke("get_checkin_config");
  },

  /** 新增或更新站点。id 为空则由后端生成 UUID。 */
  async upsertSite(site: CheckinSite): Promise<CheckinSite> {
    return await invoke("upsert_checkin_site", { site });
  },

  async deleteSite(id: string): Promise<boolean> {
    return await invoke("delete_checkin_site", { id });
  },

  async setSchedule(
    scheduleEnabled: boolean,
    scheduleHour: number,
  ): Promise<CheckinConfig> {
    return await invoke("set_checkin_schedule", {
      scheduleEnabled,
      scheduleHour,
    });
  },

  /** 执行单站签到，返回结果并已写回后端。 */
  async runSite(id: string): Promise<CheckinResult> {
    return await invoke("run_checkin_site", { id });
  },

  /** 顺序执行所有启用站点，返回 [站点 id, 结果] 列表。 */
  async runAll(): Promise<Array<[string, CheckinResult]>> {
    return await invoke("run_all_checkin_sites");
  },

  /**
   * 重新过 Cloudflare 验证并刷新缓存凭证，不发签到请求。
   * 会打开一个验证窗口；遇到交互式挑战时需用户点一下。
   */
  async refreshClearance(id: string): Promise<void> {
    await invoke("refresh_checkin_clearance", { id });
  },
};
