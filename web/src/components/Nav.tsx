import { NavLink, useLocation, useNavigate } from "react-router-dom";
import type { UserGuild } from "../lib/types";

interface NavProps {
  canManageSettings: boolean;
  canUseAdminViews: boolean;
  canUseUsageAdmin: boolean;
  canUseRetentionAdmin: boolean;
  isSystemAdmin: boolean;
  guilds: UserGuild[];
  selectedGuildId: string | null;
  settingsGuildId: string | null;
  onSelectedGuildIdChange: (guildId: string) => void;
}

function canSelectGuild(guild: UserGuild): boolean {
  return guild.is_member && guild.installed;
}

function guildOptionLabel(guild: UserGuild): string {
  if (!guild.is_member) {
    return `${guild.name}（未参加）`;
  }
  if (!guild.installed) {
    return `${guild.name}（未導入）`;
  }
  return guild.name;
}

export function Nav({
  canManageSettings,
  canUseAdminViews,
  canUseUsageAdmin,
  canUseRetentionAdmin,
  isSystemAdmin,
  guilds,
  selectedGuildId,
  settingsGuildId,
  onSelectedGuildIdChange,
}: NavProps) {
  const location = useLocation();
  const navigate = useNavigate();
  const selectableGuilds = guilds.filter(canSelectGuild);
  const shouldShowGuildSelector =
    guilds.length > 1 && selectableGuilds.length > 0;
  const selectedValue =
    selectableGuilds.find((guild) => guild.guild_id === selectedGuildId)
      ?.guild_id ??
    selectableGuilds[0]?.guild_id ??
    "";
  const settingsPath = settingsGuildId
    ? `/guilds/${encodeURIComponent(settingsGuildId)}/settings`
    : "/settings";
  const isAdminRoute = location.pathname.startsWith("/admin");
  const isSettingsRoute =
    location.pathname === "/settings" ||
    /^\/guilds\/[^/]+\/settings$/.test(location.pathname);

  return (
    <nav
      className={`app-nav${shouldShowGuildSelector ? " has-guild-selector" : ""}`}
      aria-label="Primary"
    >
      <div className="app-nav-content">
        <div className="app-nav-links">
          <NavLink
            to="/"
            end
            className={({ isActive }) =>
              `app-nav-link${isActive ? " active" : ""}`
            }
          >
            {"\u4f1a\u8b70\u4e00\u89a7"}
          </NavLink>
          {canManageSettings ? (
            <NavLink
              to={settingsPath}
              className={({ isActive }) =>
                `app-nav-link${isActive ? " active" : ""}`
              }
            >
              {"\u8a2d\u5b9a"}
            </NavLink>
          ) : null}
          {canUseAdminViews || canUseUsageAdmin || canUseRetentionAdmin ? (
            <>
              {canUseUsageAdmin ? (
                <>
                  <NavLink
                    to="/admin/usage"
                    className={({ isActive }) =>
                      `app-nav-link${isActive ? " active" : ""}`
                    }
                  >
                    {"Usage"}
                  </NavLink>
                  <NavLink
                    to="/admin/jobs"
                    className={({ isActive }) =>
                      `app-nav-link${isActive ? " active" : ""}`
                    }
                  >
                    {"Jobs"}
                  </NavLink>
                </>
              ) : null}
              {canUseAdminViews ? (
                <NavLink
                  to="/admin/audit"
                  className={({ isActive }) =>
                    `app-nav-link${isActive ? " active" : ""}`
                  }
                >
                  {"Audit"}
                </NavLink>
              ) : null}
              {canUseRetentionAdmin ? (
                <NavLink
                  to="/admin/retention"
                  className={({ isActive }) =>
                    `app-nav-link${isActive ? " active" : ""}`
                  }
                >
                  {"Retention"}
                </NavLink>
              ) : null}
            </>
          ) : null}
          {isSystemAdmin ? (
            <NavLink
              to="/admin/plans"
              className={({ isActive }) =>
                `app-nav-link${isActive ? " active" : ""}`
              }
            >
              {"Plans"}
            </NavLink>
          ) : null}
        </div>
        {shouldShowGuildSelector && !isAdminRoute ? (
          <label className="guild-selector">
            <span>{"\u30ae\u30eb\u30c9"}</span>
            <select
              aria-label="ギルド"
              value={selectedValue}
              onChange={(event) => {
                const guildId = event.target.value;
                onSelectedGuildIdChange(guildId);
                if (isSettingsRoute) {
                  navigate(`/guilds/${encodeURIComponent(guildId)}/settings`);
                }
              }}
            >
              {guilds.map((guild) => (
                <option
                  key={guild.guild_id}
                  value={guild.guild_id}
                  disabled={!canSelectGuild(guild)}
                >
                  {guildOptionLabel(guild)}
                </option>
              ))}
            </select>
          </label>
        ) : null}
      </div>
    </nav>
  );
}
