import { NavLink, useLocation, useNavigate } from "react-router-dom";
import type { UserGuild } from "../lib/types";

interface NavProps {
  isAdmin: boolean;
  guilds: UserGuild[];
  selectedGuildId: string | null;
  settingsGuildId: string | null;
  onSelectedGuildIdChange: (guildId: string) => void;
}

function canSelectGuild(guild: UserGuild): boolean {
  return guild.is_member && guild.tenant_id !== null;
}

function guildOptionLabel(guild: UserGuild): string {
  if (!guild.is_member) {
    return `${guild.name}（未参加）`;
  }
  if (guild.tenant_id === null) {
    return `${guild.name}（未導入）`;
  }
  return guild.name;
}

export function Nav({
  isAdmin,
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
          {isAdmin ? (
            <NavLink
              to={settingsPath}
              className={({ isActive }) =>
                `app-nav-link${isActive ? " active" : ""}`
              }
            >
              {"\u8a2d\u5b9a"}
            </NavLink>
          ) : null}
        </div>
        {shouldShowGuildSelector ? (
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
