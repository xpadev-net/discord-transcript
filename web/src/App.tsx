import { type ReactNode, useEffect, useState } from "react";
import { Navigate, Route, Routes, useParams } from "react-router-dom";
import { ForbiddenState } from "./components/ForbiddenState";
import { Nav } from "./components/Nav";
import { fetchMe, fetchMeGuilds } from "./lib/api";
import type { MeResponse, UserGuild } from "./lib/types";
import { AdminPlansPage } from "./pages/AdminPlansPage";
import {
  AdminAuditPage,
  AdminJobsPage,
  AdminRetentionPage,
  AdminUsagePage,
  DashboardPage,
} from "./pages/DashboardPage";
import { MeetingPage } from "./pages/MeetingPage";
import { SettingsPage } from "./pages/SettingsPage";

const selectedGuildStorageKey = "dt.selectedGuildId";

function readStoredGuildId(): string | null {
  try {
    return window.localStorage.getItem(selectedGuildStorageKey);
  } catch {
    return null;
  }
}

function storeSelectedGuildId(guildId: string): void {
  try {
    window.localStorage.setItem(selectedGuildStorageKey, guildId);
  } catch {
    // Ignore storage failures; the in-memory selection still works.
  }
}

function canSelectGuild(guild: UserGuild): boolean {
  return guild.is_member && guild.installed;
}

function canManageCurrentGuildSettings(me: MeResponse | null): boolean {
  return me?.can_manage_settings ?? me?.is_admin ?? false;
}

function canCustomizeCurrentGuildSettings(me: MeResponse | null): boolean {
  return (
    (me?.can_manage_domain_knowledge ?? false) ||
    (me?.can_manage_summary_templates ?? false) ||
    (me?.is_admin ?? false)
  );
}

function canAccessCurrentGuildSettings(me: MeResponse | null): boolean {
  return (
    canManageCurrentGuildSettings(me) || canCustomizeCurrentGuildSettings(me)
  );
}

function canViewCurrentGuildUsage(me: MeResponse | null): boolean {
  return me?.can_view_usage ?? me?.is_admin ?? false;
}

function canViewCurrentGuildAdmin(me: MeResponse | null): boolean {
  return me?.can_view_admin ?? me?.is_admin ?? false;
}

function chooseSelectedGuildId(
  me: MeResponse,
  guilds: UserGuild[],
  preferredGuildId: string | null,
): string | null {
  const selectableGuilds = guilds.filter(canSelectGuild);
  if (
    preferredGuildId &&
    selectableGuilds.some((guild) => guild.guild_id === preferredGuildId)
  ) {
    return preferredGuildId;
  }
  if (selectableGuilds.some((guild) => guild.guild_id === me.guild_id)) {
    return me.guild_id;
  }
  return selectableGuilds[0]?.guild_id ?? null;
}

export function App() {
  const [me, setMe] = useState<MeResponse | null>(null);
  const [guilds, setGuilds] = useState<UserGuild[]>([]);
  const [selectedGuildId, setSelectedGuildId] = useState<string | null>(null);
  const [guildsLoaded, setGuildsLoaded] = useState(false);
  const [guildsUnavailable, setGuildsUnavailable] = useState(false);
  const [loadingMe, setLoadingMe] = useState(true);
  const [sessionForbidden, setSessionForbidden] = useState(false);
  const [sessionError, setSessionError] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    setLoadingMe(true);
    setMe(null);
    setGuilds([]);
    setSelectedGuildId(null);
    setGuildsLoaded(false);
    setGuildsUnavailable(false);
    setSessionForbidden(false);
    setSessionError(false);

    fetchMe(controller.signal)
      .then((response) => {
        if (!controller.signal.aborted) {
          setMe(response);
        }
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        if (err instanceof Error && err.message === "forbidden") {
          setSessionForbidden(true);
        } else {
          setSessionError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoadingMe(false);
        }
      });

    return () => controller.abort();
  }, []);

  useEffect(() => {
    if (!me) {
      return;
    }
    const controller = new AbortController();
    fetchMeGuilds(controller.signal)
      .then((response) => {
        if (!controller.signal.aborted) {
          setGuilds(response);
          setGuildsUnavailable(false);
          setGuildsLoaded(true);
        }
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setGuilds([]);
          setSelectedGuildId(me.guild_id);
          storeSelectedGuildId(me.guild_id);
          setGuildsUnavailable(true);
          setGuildsLoaded(true);
        }
      });
    return () => controller.abort();
  }, [me]);

  useEffect(() => {
    if (!me || !guildsLoaded) {
      return;
    }
    if (guilds.length === 0) {
      setSelectedGuildId(me.guild_id);
      storeSelectedGuildId(me.guild_id);
      return;
    }
    setSelectedGuildId((current) => {
      const selected = chooseSelectedGuildId(
        me,
        guilds,
        current ?? readStoredGuildId(),
      );
      if (selected) {
        storeSelectedGuildId(selected);
      }
      return selected;
    });
  }, [me, guilds, guildsLoaded]);

  const loadingGuilds = me !== null && !guildsLoaded;
  const selectedGuild =
    guilds.find((guild) => guild.guild_id === selectedGuildId) ?? null;
  const noSelectableGuilds =
    guildsLoaded &&
    guilds.length > 0 &&
    guilds.every((guild) => !canSelectGuild(guild));
  const canUseSelectedGuildSettings = selectedGuild
    ? canSelectGuild(selectedGuild) &&
      (selectedGuild.guild_id === me?.guild_id
        ? canAccessCurrentGuildSettings(me)
        : true)
    : canAccessCurrentGuildSettings(me) && selectedGuildId === me?.guild_id;
  const canUseCurrentGuildAdminViews = canViewCurrentGuildAdmin(me);
  const canUseCurrentGuildUsageAdmin = canViewCurrentGuildUsage(me);
  const canUseCurrentGuildRetentionAdmin =
    canUseCurrentGuildAdminViews && canManageCurrentGuildSettings(me);
  const defaultAdminPath = canUseCurrentGuildUsageAdmin
    ? "/admin/usage"
    : canUseCurrentGuildAdminViews
      ? "/admin/audit"
      : null;
  const currentGuildName =
    guilds.find((guild) => me && guild.guild_id === me.guild_id)?.name ??
    undefined;
  const useCurrentGuildMeetings =
    guildsUnavailable && me !== null && selectedGuildId === me.guild_id;

  return (
    <>
      <Nav
        canManageSettings={canUseSelectedGuildSettings}
        canUseAdminViews={canUseCurrentGuildAdminViews}
        canUseUsageAdmin={canUseCurrentGuildUsageAdmin}
        canUseRetentionAdmin={canUseCurrentGuildRetentionAdmin}
        isSystemAdmin={me?.is_admin === true}
        guilds={guilds}
        selectedGuildId={selectedGuildId}
        settingsGuildId={selectedGuildId}
        onSelectedGuildIdChange={(guildId) => {
          setSelectedGuildId(guildId);
          storeSelectedGuildId(guildId);
        }}
      />
      <Routes>
        <Route
          path="/"
          element={
            <DashboardPage
              key={selectedGuildId ?? "no-guild"}
              selectedGuildId={selectedGuildId}
              selectedGuildName={selectedGuild?.name}
              useCurrentGuildMeetings={useCurrentGuildMeetings}
              loadingGuildSelection={
                loadingMe ||
                loadingGuilds ||
                (me !== null && selectedGuildId === null && !noSelectableGuilds)
              }
              noSelectableGuilds={noSelectableGuilds}
            />
          }
        />
        <Route
          path="/settings"
          element={
            <SettingsRoute
              canAccess={
                canAccessCurrentGuildSettings(me) &&
                selectedGuildId === me?.guild_id
              }
              loading={loadingMe || loadingGuilds}
              forbidden={sessionForbidden}
              error={sessionError}
              guildName={
                selectedGuild && me && selectedGuild.guild_id === me.guild_id
                  ? selectedGuild.name
                  : undefined
              }
            />
          }
        />
        <Route
          path="/guilds/:guildId/settings"
          element={
            <TargetSettingsRoute
              me={me}
              guilds={guilds}
              selectedGuildId={selectedGuildId}
              onSelectedGuildIdChange={(guildId) => {
                setSelectedGuildId(guildId);
                storeSelectedGuildId(guildId);
              }}
              loading={loadingMe || loadingGuilds}
              forbidden={sessionForbidden}
              error={sessionError}
            />
          }
        />
        <Route path="/meetings/:meetingId" element={<MeetingPage />} />
        <Route
          path="/admin"
          element={
            loadingMe ? (
              <GuildAdminRoute
                isAdmin={false}
                loading={loadingMe}
                forbidden={sessionForbidden}
                error={sessionError}
              />
            ) : defaultAdminPath ? (
              <Navigate to={defaultAdminPath} replace />
            ) : (
              <GuildAdminRoute
                isAdmin={false}
                loading={false}
                forbidden={sessionForbidden}
                error={sessionError}
              />
            )
          }
        />
        <Route
          path="/admin/usage"
          element={
            <GuildAdminRoute
              isAdmin={canUseCurrentGuildUsageAdmin}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            >
              <AdminUsagePage
                selectedGuildName={currentGuildName}
                isSystemAdmin={me?.is_admin === true}
                canManageSettings={canManageCurrentGuildSettings(me)}
              />
            </GuildAdminRoute>
          }
        />
        <Route
          path="/admin/jobs"
          element={
            <GuildAdminRoute
              isAdmin={canUseCurrentGuildUsageAdmin}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            >
              <AdminJobsPage selectedGuildName={currentGuildName} />
            </GuildAdminRoute>
          }
        />
        <Route
          path="/admin/audit"
          element={
            <GuildAdminRoute
              isAdmin={canUseCurrentGuildAdminViews}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            >
              <AdminAuditPage
                selectedGuildName={currentGuildName}
                isSystemAdmin={me?.is_admin === true}
              />
            </GuildAdminRoute>
          }
        />
        <Route
          path="/admin/retention"
          element={
            <GuildAdminRoute
              isAdmin={canUseCurrentGuildRetentionAdmin}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            >
              <AdminRetentionPage selectedGuildName={currentGuildName} />
            </GuildAdminRoute>
          }
        />
        <Route
          path="/admin/plans"
          element={
            <SystemAdminRoute
              isAdmin={me?.is_admin === true}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            />
          }
        />
        <Route
          path="*"
          element={
            <div className="empty-state">
              {
                "\u4f1a\u8b70\u3092\u9078\u629e\u3057\u3066\u304f\u3060\u3055\u3044"
              }
            </div>
          }
        />
      </Routes>
    </>
  );
}

interface AdminRouteProps {
  isAdmin: boolean;
  loading: boolean;
  forbidden: boolean;
  error: boolean;
  children?: ReactNode;
}

function AdminRouteFrame({
  isAdmin,
  loading,
  forbidden,
  error,
  forbiddenMessage,
  children,
}: AdminRouteProps & { forbiddenMessage: string }) {
  if (loading) {
    return (
      <main className="admin-page">
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
        </output>
      </main>
    );
  }

  if (error) {
    return (
      <main className="admin-page">
        <div className="panel-error settings-panel-message" role="alert">
          {
            "\u6a29\u9650\u60c5\u5831\u3092\u78ba\u8a8d\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f"
          }
        </div>
      </main>
    );
  }

  if (forbidden || !isAdmin) {
    return (
      <main className="admin-page">
        <ForbiddenState message={forbiddenMessage} />
      </main>
    );
  }

  return <>{children}</>;
}

function GuildAdminRoute(props: AdminRouteProps) {
  return (
    <AdminRouteFrame
      {...props}
      forbiddenMessage="このページを表示する権限がありません"
    />
  );
}

function SystemAdminRoute(props: AdminRouteProps) {
  return (
    <AdminRouteFrame {...props} forbiddenMessage="システム管理権限が必要です">
      <AdminPlansPage />
    </AdminRouteFrame>
  );
}

interface SettingsRouteProps {
  canAccess: boolean;
  loading: boolean;
  forbidden: boolean;
  error: boolean;
  guildName?: string;
}

function SettingsRoute({
  canAccess,
  loading,
  forbidden,
  error,
  guildName,
}: SettingsRouteProps) {
  if (loading) {
    return (
      <main className="settings-page">
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
        </output>
      </main>
    );
  }

  if (error) {
    return (
      <main className="settings-page">
        <div className="panel-error settings-panel-message" role="alert">
          {
            "\u6a29\u9650\u60c5\u5831\u3092\u78ba\u8a8d\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f"
          }
        </div>
      </main>
    );
  }

  if (forbidden || !canAccess) {
    return (
      <main className="settings-page">
        <ForbiddenState />
      </main>
    );
  }

  return <SettingsPage guildName={guildName} />;
}

interface TargetSettingsRouteProps {
  me: MeResponse | null;
  guilds: UserGuild[];
  selectedGuildId: string | null;
  onSelectedGuildIdChange: (guildId: string) => void;
  loading: boolean;
  forbidden: boolean;
  error: boolean;
}

function TargetSettingsRoute({
  me,
  guilds,
  selectedGuildId,
  onSelectedGuildIdChange,
  loading,
  forbidden,
  error,
}: TargetSettingsRouteProps) {
  const { guildId } = useParams();
  const targetGuild =
    guilds.find((guild) => guild.guild_id === guildId) ?? null;
  const selectableGuilds = guilds.filter(canSelectGuild);
  const canFallbackToCurrentGuild =
    guilds.length === 0 && me !== null && guildId === me.guild_id;
  const targetIsSelectable =
    targetGuild !== null
      ? canSelectGuild(targetGuild)
      : canFallbackToCurrentGuild;
  const targetIsAdmin =
    targetGuild !== null
      ? canSelectGuild(targetGuild)
      : canFallbackToCurrentGuild && canAccessCurrentGuildSettings(me);

  useEffect(() => {
    if (
      guildId &&
      targetGuild &&
      canSelectGuild(targetGuild) &&
      selectedGuildId !== guildId
    ) {
      onSelectedGuildIdChange(guildId);
    }
  }, [guildId, onSelectedGuildIdChange, selectedGuildId, targetGuild]);

  if (loading) {
    return (
      <main className="settings-page">
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
        </output>
      </main>
    );
  }

  if (error || !guildId) {
    return (
      <main className="settings-page">
        <div className="panel-error settings-panel-message" role="alert">
          {
            "\u6a29\u9650\u60c5\u5831\u3092\u78ba\u8a8d\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f"
          }
        </div>
      </main>
    );
  }

  if (selectableGuilds.length === 0 && guilds.length > 0) {
    return (
      <main className="settings-page">
        <div className="empty-state">
          {
            "\u8a2d\u5b9a\u3067\u304d\u308b\u30ae\u30eb\u30c9\u304c\u3042\u308a\u307e\u305b\u3093"
          }
        </div>
      </main>
    );
  }

  if (forbidden || !targetIsSelectable || !targetIsAdmin) {
    return (
      <main className="settings-page">
        <ForbiddenState message="\u3053\u306e\u30ae\u30eb\u30c9\u8a2d\u5b9a\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093" />
      </main>
    );
  }

  return (
    <SettingsPage
      guildId={guildId}
      guildName={targetGuild?.name ?? guildId}
      showCustomizations={me?.guild_id === guildId}
    />
  );
}
