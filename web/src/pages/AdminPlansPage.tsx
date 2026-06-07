import {
  type FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  archiveAdminGuildPlanAssignment,
  archiveAdminPlan,
  createAdminGuildPlanAssignment,
  createAdminPlan,
  createAdminPlanQuota,
  deleteAdminPlanQuota,
  fetchAdminBetaPlan,
  fetchAdminDefaultPlan,
  fetchAdminGuildPlanAssignments,
  fetchAdminPlans,
  updateAdminGuildPlanAssignment,
  updateAdminPlan,
  updateAdminPlanQuota,
} from "../lib/api";
import type {
  AdminGuildPlanAssignment,
  AdminGuildPlanAssignmentCreateRequest,
  AdminGuildPlanAssignmentSource,
  AdminGuildPlanAssignmentUpsertRequest,
  AdminPlan,
  AdminPlanKind,
  AdminPlanQuota,
  AdminPlanQuotaUpsertRequest,
  AdminPlanStatus,
  AdminPlanUpsertRequest,
  AdminQuotaDimension,
  AdminQuotaEnforcementMode,
  AdminQuotaPeriod,
} from "../lib/types";

type AdminOperation =
  | "load"
  | "plan-save"
  | "plan-archive"
  | "quota-save"
  | "quota-delete"
  | "assignment-save"
  | "assignment-archive";

interface PlanDraft {
  id: string | null;
  code: string;
  name: string;
  kind: AdminPlanKind;
  status: AdminPlanStatus;
}

interface QuotaDraft {
  id: string | null;
  dimension: AdminQuotaDimension;
  period: AdminQuotaPeriod;
  limit_value: string;
  unlimited: boolean;
  enforcement_mode: AdminQuotaEnforcementMode;
}

interface AssignmentDraft {
  id: string | null;
  tenant_id: string;
  guild_id: string;
  plan_id: string;
  valid_from: string;
  valid_until: string;
  assigned_by_user_id: string;
  source: AdminGuildPlanAssignmentSource;
}

interface AssignmentFilters {
  guildId: string;
  tenantId: string;
  includeArchived: boolean;
  limit: string;
}

const planKinds: AdminPlanKind[] = ["default", "beta", "custom"];
const planStatuses: AdminPlanStatus[] = ["active", "archived"];
const quotaDimensions: AdminQuotaDimension[] = [
  "recording_minutes",
  "asr_seconds",
  "summary_runs",
  "storage_bytes",
  "debug_downloads",
];
const quotaPeriods: AdminQuotaPeriod[] = [
  "daily",
  "monthly",
  "total",
  "current",
];
const enforcementModes: AdminQuotaEnforcementMode[] = [
  "observe_only",
  "enforce",
];
const assignmentSources: AdminGuildPlanAssignmentSource[] = [
  "system",
  "admin",
  "billing_provider",
  "migration",
];

const planKindLabels: Record<AdminPlanKind, string> = {
  default: "Default",
  beta: "Beta",
  custom: "Custom",
};

const planStatusLabels: Record<AdminPlanStatus, string> = {
  active: "Active",
  archived: "Archived",
};

const quotaDimensionLabels: Record<AdminQuotaDimension, string> = {
  recording_minutes: "Recording minutes",
  asr_seconds: "ASR seconds",
  summary_runs: "Summary runs",
  storage_bytes: "Storage bytes",
  debug_downloads: "Debug downloads",
};

const quotaPeriodLabels: Record<AdminQuotaPeriod, string> = {
  daily: "Daily",
  monthly: "Monthly",
  total: "Total",
  current: "Current",
};

const enforcementModeLabels: Record<AdminQuotaEnforcementMode, string> = {
  observe_only: "Observe only",
  enforce: "Enforce",
};

const assignmentSourceLabels: Record<AdminGuildPlanAssignmentSource, string> = {
  system: "System",
  admin: "Admin",
  billing_provider: "Billing provider",
  migration: "Migration",
};

function emptyPlanDraft(): PlanDraft {
  return {
    id: null,
    code: "",
    name: "",
    kind: "custom",
    status: "active",
  };
}

function planDraftFromPlan(plan: AdminPlan): PlanDraft {
  return {
    id: plan.id,
    code: plan.code,
    name: plan.name,
    kind: plan.kind,
    status: plan.status,
  };
}

function emptyQuotaDraft(): QuotaDraft {
  return {
    id: null,
    dimension: "recording_minutes",
    period: "monthly",
    limit_value: "",
    unlimited: false,
    enforcement_mode: "observe_only",
  };
}

function quotaDraftFromQuota(quota: AdminPlanQuota): QuotaDraft {
  return {
    id: quota.id,
    dimension: quota.dimension,
    period: quota.period,
    limit_value: quota.limit_value === null ? "" : String(quota.limit_value),
    unlimited: quota.unlimited,
    enforcement_mode: quota.enforcement_mode,
  };
}

function emptyAssignmentDraft(planId = ""): AssignmentDraft {
  return {
    id: null,
    tenant_id: "",
    guild_id: "",
    plan_id: planId,
    valid_from: localDateTimeValue(new Date().toISOString()),
    valid_until: "",
    assigned_by_user_id: "",
    source: "admin",
  };
}

function assignmentDraftFromAssignment(
  assignment: AdminGuildPlanAssignment,
): AssignmentDraft {
  return {
    id: assignment.id,
    tenant_id: assignment.tenant_id,
    guild_id: assignment.guild_id,
    plan_id: assignment.plan_id,
    valid_from: localDateTimeValue(assignment.valid_from),
    valid_until: assignment.valid_until
      ? localDateTimeValue(assignment.valid_until)
      : "",
    assigned_by_user_id: assignment.assigned_by_user_id ?? "",
    source: assignment.source,
  };
}

function emptyAssignmentFilters(): AssignmentFilters {
  return {
    guildId: "",
    tenantId: "",
    includeArchived: false,
    limit: "50",
  };
}

function localDateTimeValue(isoValue: string): string {
  const date = new Date(isoValue);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const offsetMs = date.getTimezoneOffset() * 60_000;
  return new Date(date.getTime() - offsetMs).toISOString().slice(0, 16);
}

function dateTimeLocalToIso(value: string): string {
  return new Date(value).toISOString();
}

function optionalDateTimeLocalToIso(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? dateTimeLocalToIso(trimmed) : null;
}

function adminErrorMessage(err: unknown, fallback: string): string {
  if (!(err instanceof Error)) {
    return fallback;
  }
  if (err.message.startsWith("401")) {
    return "管理トークンを確認してください";
  }
  if (err.message.startsWith("403")) {
    return "システム管理者権限が必要です";
  }
  if (err.message.startsWith("400")) {
    return "入力値を確認してください";
  }
  if (err.message.startsWith("404")) {
    return "対象が見つかりませんでした";
  }
  if (err.message.startsWith("409")) {
    return "同じコードまたは割り当てが既に存在します";
  }
  return fallback;
}

function formatLimit(quota: AdminPlanQuota): string {
  return quota.unlimited ? "Unlimited" : String(quota.limit_value ?? "");
}

function formatAssignmentWindow(assignment: AdminGuildPlanAssignment): string {
  const until = assignment.valid_until ?? "no end";
  return `${assignment.valid_from} -> ${until}`;
}

function isArchiveConfirmKey(action: string, id: string | null): string {
  return `${action}:${id ?? "new"}`;
}

function canRunConfirmedAction(armedAt: number | null): boolean {
  return armedAt !== null && Date.now() - armedAt >= 700;
}

export function AdminPlansPage() {
  const mountedRef = useRef(true);
  const refreshRequestIdRef = useRef(0);
  const [tokenInput, setTokenInput] = useState("");
  const [adminToken, setAdminToken] = useState("");
  const [plans, setPlans] = useState<AdminPlan[]>([]);
  const [defaultPlan, setDefaultPlan] = useState<AdminPlan | null>(null);
  const [betaPlan, setBetaPlan] = useState<AdminPlan | null>(null);
  const [assignments, setAssignments] = useState<AdminGuildPlanAssignment[]>(
    [],
  );
  const [selectedPlanId, setSelectedPlanId] = useState<string | null>(null);
  const [planDraft, setPlanDraft] = useState<PlanDraft>(emptyPlanDraft);
  const [quotaDraft, setQuotaDraft] = useState<QuotaDraft>(emptyQuotaDraft);
  const [assignmentDraft, setAssignmentDraft] = useState<AssignmentDraft>(() =>
    emptyAssignmentDraft(),
  );
  const [assignmentFilters, setAssignmentFilters] = useState<AssignmentFilters>(
    emptyAssignmentFilters,
  );
  const [activeOperation, setActiveOperation] = useState<AdminOperation | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [confirmKey, setConfirmKey] = useState<string | null>(null);
  const [confirmArmedAt, setConfirmArmedAt] = useState<number | null>(null);

  const selectedPlan = useMemo(
    () => plans.find((plan) => plan.id === selectedPlanId) ?? null,
    [plans, selectedPlanId],
  );

  const activeAssignments = assignments.filter(
    (assignment) => assignment.status === "active",
  );
  const saving = activeOperation !== null;
  const controlsDisabled = !adminToken || saving;
  const planArchiveConfirming =
    confirmKey === isArchiveConfirmKey("plan-archive", planDraft.id);
  const quotaDeleteConfirming =
    confirmKey === isArchiveConfirmKey("quota-delete", quotaDraft.id);
  const assignmentArchiveConfirming =
    confirmKey ===
    isArchiveConfirmKey("assignment-archive", assignmentDraft.id);

  const refreshAdminData = useCallback(
    async (
      token: string,
      signal?: AbortSignal,
      filters: AssignmentFilters = emptyAssignmentFilters(),
    ) => {
      const requestId = refreshRequestIdRef.current + 1;
      refreshRequestIdRef.current = requestId;
      const canApply = () =>
        mountedRef.current &&
        refreshRequestIdRef.current === requestId &&
        !signal?.aborted;
      setActiveOperation("load");
      setError(null);
      try {
        const options = { bearerToken: token, signal };
        const [planList, defaultResult, betaResult, assignmentList] =
          await Promise.all([
            fetchAdminPlans(options),
            fetchAdminDefaultPlan(options).catch(() => null),
            fetchAdminBetaPlan(options).catch(() => null),
            fetchAdminGuildPlanAssignments({
              ...assignmentQueryFromFilters(filters),
              bearerToken: token,
              signal,
            }),
          ]);
        if (!canApply()) {
          return;
        }
        setPlans(planList);
        setDefaultPlan(defaultResult);
        setBetaPlan(betaResult);
        setAssignments(assignmentList);
        setSelectedPlanId((current) => {
          if (current && planList.some((plan) => plan.id === current)) {
            return current;
          }
          return planList[0]?.id ?? null;
        });
        setAssignmentDraft((current) => ({
          ...current,
          plan_id: current.plan_id || planList[0]?.id || "",
        }));
      } catch (err) {
        if (canApply()) {
          setError(adminErrorMessage(err, "管理情報の読み込みに失敗しました"));
        }
      } finally {
        if (canApply()) {
          setActiveOperation(null);
        }
      }
    },
    [],
  );

  useEffect(() => {
    document.title = "プラン管理";
    return () => {
      mountedRef.current = false;
      refreshRequestIdRef.current += 1;
    };
  }, []);

  useEffect(() => {
    if (!selectedPlan) {
      setPlanDraft(emptyPlanDraft());
      setQuotaDraft(emptyQuotaDraft());
      return;
    }
    setPlanDraft(planDraftFromPlan(selectedPlan));
    setQuotaDraft(emptyQuotaDraft());
  }, [selectedPlan]);

  function applyToken(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const token = tokenInput.trim();
    if (!token) {
      setError("管理トークンを入力してください");
      return;
    }
    setAdminToken(token);
    setError(null);
    setMessage("管理トークンを適用しました");
    setConfirmKey(null);
    setConfirmArmedAt(null);
    void refreshAdminData(token, undefined, assignmentFilters);
  }

  function selectPlan(planId: string) {
    const plan = plans.find((item) => item.id === planId);
    setSelectedPlanId(plan?.id ?? null);
    setPlanDraft(plan ? planDraftFromPlan(plan) : emptyPlanDraft());
    setQuotaDraft(emptyQuotaDraft());
    setMessage(null);
    setError(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
  }

  function startNewPlan() {
    setSelectedPlanId(null);
    setPlanDraft(emptyPlanDraft());
    setQuotaDraft(emptyQuotaDraft());
    setMessage(null);
    setError(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
  }

  function selectQuota(quotaId: string) {
    const quota = selectedPlan?.quotas.find((item) => item.id === quotaId);
    setQuotaDraft(quota ? quotaDraftFromQuota(quota) : emptyQuotaDraft());
    setMessage(null);
    setError(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
  }

  function selectAssignment(assignmentId: string) {
    const assignment = assignments.find((item) => item.id === assignmentId);
    setAssignmentDraft(
      assignment
        ? assignmentDraftFromAssignment(assignment)
        : emptyAssignmentDraft(selectedPlan?.id ?? plans[0]?.id ?? ""),
    );
    setMessage(null);
    setError(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
  }

  async function handlePlanSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!adminToken || saving) {
      return;
    }
    const validationError = validatePlanDraft(planDraft);
    if (validationError) {
      setError(validationError);
      setMessage(null);
      return;
    }
    setActiveOperation("plan-save");
    setError(null);
    setMessage(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
    try {
      const request = planRequestFromDraft(planDraft);
      const updated = planDraft.id
        ? await updateAdminPlan(planDraft.id, request, {
            bearerToken: adminToken,
          })
        : await createAdminPlan(request, { bearerToken: adminToken });
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setSelectedPlanId(updated.id);
      setMessage(
        planDraft.id
          ? "プランを保存しました。監査ログに plan.update が記録されます。"
          : "プランを作成しました。監査ログに plan.create が記録されます。",
      );
    } catch (err) {
      setError(adminErrorMessage(err, "プランの保存に失敗しました"));
    } finally {
      setActiveOperation(null);
    }
  }

  async function handlePlanArchive() {
    if (!adminToken || !planDraft.id || saving) {
      return;
    }
    const key = isArchiveConfirmKey("plan-archive", planDraft.id);
    if (confirmKey !== key) {
      setConfirmKey(key);
      setConfirmArmedAt(Date.now());
      setMessage("プランのアーカイブ確認を有効にしました。");
      setError(null);
      return;
    }
    if (!canRunConfirmedAction(confirmArmedAt)) {
      setMessage("確認ボタンが有効になってからもう一度押してください。");
      return;
    }
    setActiveOperation("plan-archive");
    setError(null);
    setMessage(null);
    try {
      const updated = await archiveAdminPlan(planDraft.id, {
        bearerToken: adminToken,
      });
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setSelectedPlanId(updated.id);
      setMessage(
        "プランをアーカイブしました。監査ログに plan.archive が記録されます。",
      );
    } catch (err) {
      setError(adminErrorMessage(err, "プランのアーカイブに失敗しました"));
    } finally {
      setConfirmKey(null);
      setConfirmArmedAt(null);
      setActiveOperation(null);
    }
  }

  async function handleQuotaSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!adminToken || !selectedPlan || saving) {
      return;
    }
    const validationError = validateQuotaDraft(quotaDraft);
    if (validationError) {
      setError(validationError);
      setMessage(null);
      return;
    }
    setActiveOperation("quota-save");
    setError(null);
    setMessage(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
    try {
      const request = quotaRequestFromDraft(quotaDraft);
      const updated = quotaDraft.id
        ? await updateAdminPlanQuota(quotaDraft.id, request, {
            bearerToken: adminToken,
          })
        : await createAdminPlanQuota(selectedPlan.id, request, {
            bearerToken: adminToken,
          });
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setQuotaDraft(quotaDraftFromQuota(updated));
      setMessage(
        quotaDraft.id
          ? "クォータを保存しました。監査ログに plan_quota.update が記録されます。"
          : "クォータを追加しました。監査ログに plan_quota.create が記録されます。",
      );
    } catch (err) {
      setError(adminErrorMessage(err, "クォータの保存に失敗しました"));
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleQuotaDelete() {
    if (!adminToken || !quotaDraft.id || saving) {
      return;
    }
    const key = isArchiveConfirmKey("quota-delete", quotaDraft.id);
    if (confirmKey !== key) {
      setConfirmKey(key);
      setConfirmArmedAt(Date.now());
      setMessage("クォータ削除の確認を有効にしました。");
      setError(null);
      return;
    }
    if (!canRunConfirmedAction(confirmArmedAt)) {
      setMessage("確認ボタンが有効になってからもう一度押してください。");
      return;
    }
    setActiveOperation("quota-delete");
    setError(null);
    setMessage(null);
    try {
      await deleteAdminPlanQuota(quotaDraft.id, { bearerToken: adminToken });
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setQuotaDraft(emptyQuotaDraft());
      setMessage(
        "クォータを削除しました。監査ログに plan_quota.delete が記録されます。",
      );
    } catch (err) {
      setError(adminErrorMessage(err, "クォータの削除に失敗しました"));
    } finally {
      setConfirmKey(null);
      setConfirmArmedAt(null);
      setActiveOperation(null);
    }
  }

  async function refreshAssignmentsOnly(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!adminToken || saving) {
      return;
    }
    const requestId = refreshRequestIdRef.current + 1;
    refreshRequestIdRef.current = requestId;
    const canApply = () =>
      mountedRef.current && refreshRequestIdRef.current === requestId;
    setActiveOperation("load");
    setError(null);
    try {
      const assignmentList = await fetchAdminGuildPlanAssignments({
        ...assignmentQueryFromFilters(assignmentFilters),
        bearerToken: adminToken,
      });
      if (!canApply()) {
        return;
      }
      setAssignments(assignmentList);
      setMessage("割り当て一覧を更新しました");
    } catch (err) {
      if (canApply()) {
        setError(
          adminErrorMessage(err, "割り当て一覧の読み込みに失敗しました"),
        );
      }
    } finally {
      if (canApply()) {
        setActiveOperation(null);
      }
    }
  }

  async function handleAssignmentSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!adminToken || saving) {
      return;
    }
    const validationError = validateAssignmentDraft(assignmentDraft);
    if (validationError) {
      setError(validationError);
      setMessage(null);
      return;
    }
    setActiveOperation("assignment-save");
    setError(null);
    setMessage(null);
    setConfirmKey(null);
    setConfirmArmedAt(null);
    try {
      const request = assignmentRequestFromDraft(assignmentDraft);
      const updated = assignmentDraft.id
        ? await updateAdminGuildPlanAssignment(assignmentDraft.id, request, {
            bearerToken: adminToken,
          })
        : await createAdminGuildPlanAssignment(
            request as AdminGuildPlanAssignmentCreateRequest,
            { bearerToken: adminToken },
          );
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setAssignmentDraft(assignmentDraftFromAssignment(updated));
      setMessage(
        assignmentDraft.id
          ? "ギルド割り当てを保存しました。監査ログに guild_plan_assignment.update が記録されます。"
          : "ギルド割り当てを作成しました。監査ログに guild_plan_assignment.create が記録されます。",
      );
    } catch (err) {
      setError(adminErrorMessage(err, "ギルド割り当ての保存に失敗しました"));
    } finally {
      setActiveOperation(null);
    }
  }

  async function handleAssignmentArchive() {
    if (!adminToken || !assignmentDraft.id || saving) {
      return;
    }
    const key = isArchiveConfirmKey("assignment-archive", assignmentDraft.id);
    if (confirmKey !== key) {
      setConfirmKey(key);
      setConfirmArmedAt(Date.now());
      setMessage("ギルド割り当てアーカイブの確認を有効にしました。");
      setError(null);
      return;
    }
    if (!canRunConfirmedAction(confirmArmedAt)) {
      setMessage("確認ボタンが有効になってからもう一度押してください。");
      return;
    }
    setActiveOperation("assignment-archive");
    setError(null);
    setMessage(null);
    try {
      const updated = await archiveAdminGuildPlanAssignment(
        assignmentDraft.id,
        {
          bearerToken: adminToken,
        },
      );
      await refreshAdminData(adminToken, undefined, assignmentFilters);
      setAssignmentDraft(assignmentDraftFromAssignment(updated));
      setMessage(
        "ギルド割り当てをアーカイブしました。監査ログに guild_plan_assignment.archive が記録されます。",
      );
    } catch (err) {
      setError(
        adminErrorMessage(err, "ギルド割り当てのアーカイブに失敗しました"),
      );
    } finally {
      setConfirmKey(null);
      setConfirmArmedAt(null);
      setActiveOperation(null);
    }
  }

  return (
    <main className="admin-page">
      <div className="settings-header">
        <div>
          <h1>プラン管理</h1>
          <p>
            プラン、クォータ、ギルド割り当てをシステム管理者として管理します
          </p>
        </div>
      </div>

      <form className="admin-token-bar" onSubmit={applyToken}>
        <label className="settings-field">
          <span>管理トークン</span>
          <input
            className="admin-token-input"
            type="password"
            autoComplete="off"
            value={tokenInput}
            onChange={(event) => setTokenInput(event.target.value)}
          />
        </label>
        <button className="primary-button" type="submit" disabled={saving}>
          適用
        </button>
      </form>

      {error ? (
        <div className="settings-inline-error" role="alert">
          {error}
        </div>
      ) : null}
      {message ? <output className="settings-success">{message}</output> : null}
      {activeOperation === "load" ? (
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          読み込み中
        </output>
      ) : null}

      {!adminToken ? (
        <div className="settings-notice">
          管理 API は Bearer
          トークンが必要です。トークンはブラウザには保存しません。
        </div>
      ) : null}

      {adminToken ? (
        <div className="admin-layout">
          <section className="settings-section">
            <div className="settings-section-heading">
              <div>
                <h2>既定プラン</h2>
                <p>Default / Beta プランを API から確認します。</p>
              </div>
              <button
                className="secondary-button"
                type="button"
                disabled={saving}
                onClick={() =>
                  void refreshAdminData(
                    adminToken,
                    undefined,
                    assignmentFilters,
                  )
                }
              >
                再読み込み
              </button>
            </div>
            <div className="admin-plan-summary-grid">
              <PlanSummary title="Default" plan={defaultPlan} />
              <PlanSummary title="Beta" plan={betaPlan} />
            </div>
          </section>

          <section className="settings-section">
            <div className="settings-section-heading">
              <div>
                <h2>プラン</h2>
                <p>作成、更新、アーカイブは監査ログに記録されます。</p>
              </div>
              <button
                className="secondary-button"
                type="button"
                onClick={startNewPlan}
                disabled={saving}
              >
                新規
              </button>
            </div>
            <label className="settings-field">
              <span>プラン選択</span>
              <select
                value={selectedPlanId ?? ""}
                onChange={(event) => selectPlan(event.target.value)}
                disabled={saving || plans.length === 0}
              >
                {plans.length === 0 ? (
                  <option value="">プランがありません</option>
                ) : null}
                {plans.map((plan) => (
                  <option key={plan.id} value={plan.id}>
                    {plan.name} ({plan.code})
                  </option>
                ))}
              </select>
            </label>
            <form className="admin-form-grid" onSubmit={handlePlanSubmit}>
              {!planDraft.id ? (
                <label className="settings-field">
                  <span>ID (任意)</span>
                  <input
                    type="text"
                    value=""
                    placeholder="未指定なら自動生成"
                    disabled
                  />
                </label>
              ) : (
                <ReadOnlyField label="ID" value={planDraft.id} />
              )}
              <label className="settings-field">
                <span>コード</span>
                <input
                  type="text"
                  value={planDraft.code}
                  onChange={(event) =>
                    setPlanDraft((current) => ({
                      ...current,
                      code: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled}
                />
              </label>
              <label className="settings-field">
                <span>名前</span>
                <input
                  type="text"
                  value={planDraft.name}
                  onChange={(event) =>
                    setPlanDraft((current) => ({
                      ...current,
                      name: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled}
                />
              </label>
              <label className="settings-field">
                <span>種別</span>
                <select
                  value={planDraft.kind}
                  onChange={(event) =>
                    setPlanDraft((current) => ({
                      ...current,
                      kind: event.target.value as AdminPlanKind,
                    }))
                  }
                  disabled={controlsDisabled}
                >
                  {planKinds.map((kind) => (
                    <option key={kind} value={kind}>
                      {planKindLabels[kind]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="settings-field">
                <span>ステータス</span>
                <select
                  value={planDraft.status}
                  onChange={(event) =>
                    setPlanDraft((current) => ({
                      ...current,
                      status: event.target.value as AdminPlanStatus,
                    }))
                  }
                  disabled={controlsDisabled}
                >
                  {planStatuses.map((status) => (
                    <option key={status} value={status}>
                      {planStatusLabels[status]}
                    </option>
                  ))}
                </select>
              </label>
              <div className="admin-actions">
                <button
                  className="primary-button"
                  type="submit"
                  disabled={controlsDisabled}
                >
                  {planDraft.id ? "保存" : "作成"}
                </button>
                <button
                  className="secondary-button danger-button"
                  type="button"
                  disabled={
                    controlsDisabled ||
                    !planDraft.id ||
                    selectedPlan?.status === "archived"
                  }
                  onClick={() => void handlePlanArchive()}
                >
                  {planArchiveConfirming ? "確認してアーカイブ" : "アーカイブ"}
                </button>
              </div>
              {planArchiveConfirming ? (
                <p className="admin-confirm-note">
                  プランをアーカイブします。関連するギルド割り当てへの影響を確認してください。
                </p>
              ) : null}
            </form>
          </section>

          <section className="settings-section">
            <div className="settings-section-heading">
              <div>
                <h2>クォータ</h2>
                <p>Unlimited を有効にすると limit_value は送信しません。</p>
              </div>
              <button
                className="secondary-button"
                type="button"
                onClick={() => setQuotaDraft(emptyQuotaDraft())}
                disabled={saving || !selectedPlan}
              >
                新規
              </button>
            </div>
            <QuotaList
              quotas={selectedPlan?.quotas ?? []}
              selectedQuotaId={quotaDraft.id}
              onSelect={selectQuota}
            />
            <form className="admin-form-grid" onSubmit={handleQuotaSubmit}>
              <label className="settings-field">
                <span>対象プラン</span>
                <input
                  type="text"
                  value={selectedPlan?.name ?? "プランを選択してください"}
                  disabled
                />
              </label>
              <label className="settings-field">
                <span>ディメンション</span>
                <select
                  value={quotaDraft.dimension}
                  onChange={(event) =>
                    setQuotaDraft((current) => ({
                      ...current,
                      dimension: event.target.value as AdminQuotaDimension,
                    }))
                  }
                  disabled={controlsDisabled || !selectedPlan}
                >
                  {quotaDimensions.map((dimension) => (
                    <option key={dimension} value={dimension}>
                      {quotaDimensionLabels[dimension]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="settings-field">
                <span>期間</span>
                <select
                  value={quotaDraft.period}
                  onChange={(event) =>
                    setQuotaDraft((current) => ({
                      ...current,
                      period: event.target.value as AdminQuotaPeriod,
                    }))
                  }
                  disabled={controlsDisabled || !selectedPlan}
                >
                  {quotaPeriods.map((period) => (
                    <option key={period} value={period}>
                      {quotaPeriodLabels[period]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="settings-checkbox admin-checkbox-inline">
                <input
                  type="checkbox"
                  checked={quotaDraft.unlimited}
                  onChange={(event) =>
                    setQuotaDraft((current) => ({
                      ...current,
                      unlimited: event.target.checked,
                      limit_value: event.target.checked
                        ? ""
                        : current.limit_value,
                    }))
                  }
                  disabled={controlsDisabled || !selectedPlan}
                />
                <span>Unlimited</span>
              </label>
              <label className="settings-field">
                <span>上限値</span>
                <input
                  type="number"
                  min={0}
                  value={quotaDraft.limit_value}
                  onChange={(event) =>
                    setQuotaDraft((current) => ({
                      ...current,
                      limit_value: event.target.value,
                    }))
                  }
                  disabled={
                    controlsDisabled || !selectedPlan || quotaDraft.unlimited
                  }
                />
              </label>
              <label className="settings-field">
                <span>適用モード</span>
                <select
                  value={quotaDraft.enforcement_mode}
                  onChange={(event) =>
                    setQuotaDraft((current) => ({
                      ...current,
                      enforcement_mode: event.target
                        .value as AdminQuotaEnforcementMode,
                    }))
                  }
                  disabled={controlsDisabled || !selectedPlan}
                >
                  {enforcementModes.map((mode) => (
                    <option key={mode} value={mode}>
                      {enforcementModeLabels[mode]}
                    </option>
                  ))}
                </select>
              </label>
              <div className="admin-actions">
                <button
                  className="primary-button"
                  type="submit"
                  disabled={controlsDisabled || !selectedPlan}
                >
                  {quotaDraft.id ? "保存" : "追加"}
                </button>
                <button
                  className="secondary-button danger-button"
                  type="button"
                  disabled={controlsDisabled || !quotaDraft.id}
                  onClick={() => void handleQuotaDelete()}
                >
                  {quotaDeleteConfirming ? "確認して削除" : "削除"}
                </button>
              </div>
              {quotaDeleteConfirming ? (
                <p className="admin-confirm-note">
                  選択中のクォータを削除します。この操作は監査ログに記録されます。
                </p>
              ) : null}
            </form>
          </section>

          <section className="settings-section admin-wide-section">
            <div className="settings-section-heading">
              <div>
                <h2>ギルド割り当て</h2>
                <p>作成、更新、アーカイブは有効プランの適用に影響します。</p>
              </div>
              <button
                className="secondary-button"
                type="button"
                disabled={saving}
                onClick={() =>
                  setAssignmentDraft(
                    emptyAssignmentDraft(
                      selectedPlan?.id ?? plans[0]?.id ?? "",
                    ),
                  )
                }
              >
                新規
              </button>
            </div>
            <form
              className="admin-filter-row"
              onSubmit={refreshAssignmentsOnly}
            >
              <label className="settings-field">
                <span>Guild ID</span>
                <input
                  className="admin-filter-input"
                  type="text"
                  value={assignmentFilters.guildId}
                  onChange={(event) =>
                    setAssignmentFilters((current) => ({
                      ...current,
                      guildId: event.target.value,
                    }))
                  }
                  disabled={saving}
                />
              </label>
              <label className="settings-field">
                <span>Tenant ID</span>
                <input
                  className="admin-filter-input"
                  type="text"
                  value={assignmentFilters.tenantId}
                  onChange={(event) =>
                    setAssignmentFilters((current) => ({
                      ...current,
                      tenantId: event.target.value,
                    }))
                  }
                  disabled={saving}
                />
              </label>
              <label className="settings-field admin-limit-field">
                <span>Limit</span>
                <input
                  className="admin-limit-input"
                  type="number"
                  min={1}
                  max={200}
                  value={assignmentFilters.limit}
                  onChange={(event) =>
                    setAssignmentFilters((current) => ({
                      ...current,
                      limit: event.target.value,
                    }))
                  }
                  disabled={saving}
                />
              </label>
              <label className="settings-checkbox admin-checkbox-inline">
                <input
                  type="checkbox"
                  checked={assignmentFilters.includeArchived}
                  onChange={(event) =>
                    setAssignmentFilters((current) => ({
                      ...current,
                      includeArchived: event.target.checked,
                    }))
                  }
                  disabled={saving}
                />
                <span>失効済みを含む</span>
              </label>
              <button
                className="secondary-button"
                type="submit"
                disabled={controlsDisabled}
              >
                絞り込み
              </button>
            </form>
            <AssignmentList
              assignments={assignments}
              activeCount={activeAssignments.length}
              selectedAssignmentId={assignmentDraft.id}
              onSelect={selectAssignment}
            />
            <form className="admin-form-grid" onSubmit={handleAssignmentSubmit}>
              <label className="settings-field">
                <span>Tenant ID</span>
                <input
                  type="text"
                  value={assignmentDraft.tenant_id}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      tenant_id: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled || assignmentDraft.id !== null}
                />
              </label>
              <label className="settings-field">
                <span>Guild ID</span>
                <input
                  type="text"
                  value={assignmentDraft.guild_id}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      guild_id: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled || assignmentDraft.id !== null}
                />
              </label>
              <label className="settings-field">
                <span>プラン</span>
                <select
                  value={assignmentDraft.plan_id}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      plan_id: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled || plans.length === 0}
                >
                  {plans.map((plan) => (
                    <option key={plan.id} value={plan.id}>
                      {plan.name} ({plan.code})
                    </option>
                  ))}
                </select>
              </label>
              <label className="settings-field">
                <span>開始</span>
                <input
                  type="datetime-local"
                  value={assignmentDraft.valid_from}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      valid_from: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled}
                />
              </label>
              <label className="settings-field">
                <span>終了 (任意)</span>
                <input
                  type="datetime-local"
                  value={assignmentDraft.valid_until}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      valid_until: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled}
                />
              </label>
              <label className="settings-field">
                <span>ソース</span>
                <select
                  value={assignmentDraft.source}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      source: event.target
                        .value as AdminGuildPlanAssignmentSource,
                    }))
                  }
                  disabled={controlsDisabled}
                >
                  {assignmentSources.map((source) => (
                    <option key={source} value={source}>
                      {assignmentSourceLabels[source]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="settings-field">
                <span>担当 User ID</span>
                <input
                  type="text"
                  value={assignmentDraft.assigned_by_user_id}
                  onChange={(event) =>
                    setAssignmentDraft((current) => ({
                      ...current,
                      assigned_by_user_id: event.target.value,
                    }))
                  }
                  disabled={controlsDisabled}
                />
              </label>
              <div className="admin-actions">
                <button
                  className="primary-button"
                  type="submit"
                  disabled={controlsDisabled || plans.length === 0}
                >
                  {assignmentDraft.id ? "保存" : "作成"}
                </button>
                <button
                  className="secondary-button danger-button"
                  type="button"
                  disabled={
                    controlsDisabled ||
                    !assignmentDraft.id ||
                    assignments.find((item) => item.id === assignmentDraft.id)
                      ?.status === "revoked"
                  }
                  onClick={() => void handleAssignmentArchive()}
                >
                  {assignmentArchiveConfirming
                    ? "確認してアーカイブ"
                    : "アーカイブ"}
                </button>
              </div>
              {assignmentArchiveConfirming ? (
                <p className="admin-confirm-note">
                  選択中のギルド割り当てを失効します。対象ギルドの有効プランが変わる可能性があります。
                </p>
              ) : null}
            </form>
          </section>
        </div>
      ) : null}
    </main>
  );
}

function PlanSummary({
  title,
  plan,
}: {
  title: string;
  plan: AdminPlan | null;
}) {
  if (!plan) {
    return (
      <div className="admin-summary-card">
        <strong>{title}</strong>
        <span>未設定または取得不可</span>
      </div>
    );
  }
  return (
    <div className="admin-summary-card">
      <strong>{plan.name}</strong>
      <span>{plan.code}</span>
      <span>
        {planKindLabels[plan.kind]} / {planStatusLabels[plan.status]} /{" "}
        {plan.quotas.length} quotas
      </span>
    </div>
  );
}

function ReadOnlyField({ label, value }: { label: string; value: string }) {
  return (
    <label className="settings-field">
      <span>{label}</span>
      <input type="text" value={value} disabled />
    </label>
  );
}

function QuotaList({
  quotas,
  selectedQuotaId,
  onSelect,
}: {
  quotas: AdminPlanQuota[];
  selectedQuotaId: string | null;
  onSelect: (quotaId: string) => void;
}) {
  if (quotas.length === 0) {
    return <div className="admin-empty-inline">クォータはありません</div>;
  }
  return (
    <div className="admin-list">
      {quotas.map((quota) => (
        <button
          key={quota.id}
          className={`admin-list-item${quota.id === selectedQuotaId ? " active" : ""}`}
          type="button"
          aria-pressed={quota.id === selectedQuotaId}
          onClick={() => onSelect(quota.id)}
        >
          <strong>{quotaDimensionLabels[quota.dimension]}</strong>
          <span>
            {quotaPeriodLabels[quota.period]} / {formatLimit(quota)} /{" "}
            {enforcementModeLabels[quota.enforcement_mode]}
          </span>
        </button>
      ))}
    </div>
  );
}

function AssignmentList({
  assignments,
  activeCount,
  selectedAssignmentId,
  onSelect,
}: {
  assignments: AdminGuildPlanAssignment[];
  activeCount: number;
  selectedAssignmentId: string | null;
  onSelect: (assignmentId: string) => void;
}) {
  if (assignments.length === 0) {
    return <div className="admin-empty-inline">割り当てはありません</div>;
  }
  return (
    <div className="admin-list">
      <div className="admin-list-meta">
        {assignments.length} 件 / active {activeCount} 件
      </div>
      {assignments.map((assignment) => (
        <button
          key={assignment.id}
          className={`admin-list-item${assignment.id === selectedAssignmentId ? " active" : ""}`}
          type="button"
          aria-pressed={assignment.id === selectedAssignmentId}
          onClick={() => onSelect(assignment.id)}
        >
          <strong>
            {assignment.guild_id} {"->"} {assignment.plan_name}
          </strong>
          <span>
            {assignment.status} / {assignmentSourceLabels[assignment.source]} /{" "}
            {formatAssignmentWindow(assignment)}
          </span>
        </button>
      ))}
    </div>
  );
}

function validatePlanDraft(draft: PlanDraft): string | null {
  if (!draft.code.trim()) {
    return "プランコードを入力してください";
  }
  if (!draft.name.trim()) {
    return "プラン名を入力してください";
  }
  return null;
}

function planRequestFromDraft(draft: PlanDraft): AdminPlanUpsertRequest {
  return {
    code: draft.code.trim(),
    name: draft.name.trim(),
    kind: draft.kind,
    status: draft.status,
  };
}

function validateQuotaDraft(draft: QuotaDraft): string | null {
  if (draft.unlimited) {
    return null;
  }
  if (!draft.limit_value.trim()) {
    return "有限クォータには上限値が必要です";
  }
  const value = Number(draft.limit_value);
  if (!Number.isInteger(value) || value < 0) {
    return "上限値は 0 以上の整数で入力してください";
  }
  return null;
}

function quotaRequestFromDraft(draft: QuotaDraft): AdminPlanQuotaUpsertRequest {
  return {
    dimension: draft.dimension,
    period: draft.period,
    unlimited: draft.unlimited,
    limit_value: draft.unlimited ? null : Number(draft.limit_value),
    enforcement_mode: draft.enforcement_mode,
  };
}

function validateAssignmentDraft(draft: AssignmentDraft): string | null {
  if (!draft.id && !draft.tenant_id.trim()) {
    return "Tenant ID を入力してください";
  }
  if (!draft.id && !draft.guild_id.trim()) {
    return "Guild ID を入力してください";
  }
  if (!draft.plan_id.trim()) {
    return "プランを選択してください";
  }
  if (!draft.valid_from.trim()) {
    return "開始日時を入力してください";
  }
  if (Number.isNaN(new Date(draft.valid_from).getTime())) {
    return "開始日時を確認してください";
  }
  if (
    draft.valid_until.trim() &&
    new Date(draft.valid_until).getTime() <=
      new Date(draft.valid_from).getTime()
  ) {
    return "終了日時は開始日時より後にしてください";
  }
  if (draft.source === "admin" && !draft.assigned_by_user_id.trim()) {
    return "ソースが Admin の場合は担当 User ID が必要です";
  }
  return null;
}

function assignmentRequestFromDraft(
  draft: AssignmentDraft,
): AdminGuildPlanAssignmentUpsertRequest {
  const request: AdminGuildPlanAssignmentUpsertRequest = {
    plan_id: draft.plan_id,
    valid_from: dateTimeLocalToIso(draft.valid_from),
    valid_until: optionalDateTimeLocalToIso(draft.valid_until),
    assigned_by_user_id: draft.assigned_by_user_id.trim() || null,
    source: draft.source,
  };
  if (!draft.id) {
    request.tenant_id = draft.tenant_id.trim();
    request.guild_id = draft.guild_id.trim();
  }
  return request;
}

function assignmentQueryFromFilters(filters: AssignmentFilters): {
  guildId?: string;
  tenantId?: string;
  includeArchived?: boolean;
  limit?: number;
} {
  const limit = Number(filters.limit);
  return {
    guildId: filters.guildId.trim() || undefined,
    tenantId: filters.tenantId.trim() || undefined,
    includeArchived: filters.includeArchived,
    limit: Number.isInteger(limit) && limit > 0 ? limit : 50,
  };
}
