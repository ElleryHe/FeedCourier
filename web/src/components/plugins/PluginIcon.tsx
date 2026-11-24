import {
  Bell,
  Code2,
  Filter,
  Globe,
  Link2,
  type LucideIcon,
  Puzzle,
  Radio,
  Send,
  Shield,
  Sparkles,
  Webhook,
} from "lucide-react";

const PLUGIN_ICON_MAP: Record<string, LucideIcon> = {
  link: Link2,
  "link-2": Link2,
  url: Link2,
  shield: Shield,
  sparkles: Sparkles,
  globe: Globe,
  filter: Filter,
  code: Code2,
  webhook: Webhook,
  bell: Bell,
  send: Send,
  radio: Radio,
  puzzle: Puzzle,
};

export function resolvePluginIcon(iconName?: string | null): LucideIcon {
  if (!iconName) return Puzzle;
  return PLUGIN_ICON_MAP[iconName.trim().toLowerCase()] ?? Puzzle;
}

export function PluginIcon({
  icon,
  className = "h-4 w-4",
}: {
  icon?: string | null;
  className?: string;
}) {
  const Icon = resolvePluginIcon(icon);
  return <Icon className={className} />;
}
