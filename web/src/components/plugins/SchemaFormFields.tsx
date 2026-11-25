import {
  FieldHint,
  Input,
  Label,
  Select,
  Switch,
  Textarea,
} from "@/components/ui/primitives";
import { useI18n } from "@/i18n";
import type { Messages } from "@/i18n/en";

export interface SchemaPropertyDef {
  type?: string;
  title?: string;
  description?: string;
  placeholder?: string;
  enum?: Array<string | number>;
  format?: string;
  default?: unknown;
  items?: { type?: string };
  "x-secret"?: boolean;
}

export interface ParsedObjectSchema {
  properties: Array<[string, SchemaPropertyDef]>;
  required: ReadonlySet<string>;
}

function isPlainRecord(val: unknown): val is Record<string, unknown> {
  return typeof val === "object" && val !== null && !Array.isArray(val);
}

function isSafeHttpUrl(raw: string): boolean {
  const trimmed = raw.trim();
  if (!trimmed) return false;
  try {
    const parsed = new URL(trimmed);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function isPrivateOrLocalDefaultHost(normHost: string): boolean {
  return (
    !normHost ||
    normHost === "localhost" ||
    normHost.endsWith(".localhost") ||
    normHost.endsWith(".local") ||
    normHost.endsWith(".internal") ||
    normHost.includes(":") ||
    /^\d{1,3}(\.\d{1,3}){3}$/.test(normHost)
  );
}

/**
 * Returns true only when a `format: "uri"` schema default is a valid public HTTP(S) URL whose
 * hostname is explicitly covered by the plugin's declared `meta.allowed_hosts`.
 */
export function isUriDefaultAllowed(
  rawDefault: unknown,
  allowedHosts?: readonly string[],
): boolean {
  if (
    typeof rawDefault !== "string" ||
    !allowedHosts ||
    allowedHosts.length === 0
  ) {
    return false;
  }
  const trimmed = rawDefault.trim();
  if (!trimmed) return false;
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return false;
    }
    const norm = parsed.hostname
      .trim()
      .replace(/^\[/, "")
      .replace(/\]$/, "")
      .replace(/\.+$/, "")
      .toLowerCase();
    if (isPrivateOrLocalDefaultHost(norm)) return false;
    return allowedHosts.some((rawPattern) => {
      const pattern = rawPattern.trim().replace(/\.+$/, "").toLowerCase();
      if (!pattern) return false;
      if (pattern.startsWith("*.")) {
        const dotSuffix = pattern.slice(1);
        return norm.length > dotSuffix.length && norm.endsWith(dotSuffix);
      }
      return norm === pattern;
    });
  } catch {
    return false;
  }
}

export type SchemaFieldValue = string | boolean | null;
export type SchemaFieldValues = Record<string, SchemaFieldValue>;

const NON_SECRET_METADATA_KEYS = new Set([
  "token_id",
  "token_name",
  "token_scope",
  "token_prefix",
  "token_count",
  "tokens_count",
]);

const SENSITIVE_FIELD_KEYWORDS = [
  "token",
  "password",
  "passwd",
  "passphrase",
  "secret",
  "signature",
  "api_key",
  "api-key",
  "apikey",
  "device_key",
  "device-key",
  "push_key",
  "push-key",
  "access_key",
  "access-key",
  "webhook_key",
  "webhook-key",
  "authorization",
  "cookie",
  "credential",
  "bearer",
  "private_key",
  "private-key",
] as const;

export function isSchemaSecretProperty(
  key: string,
  prop?: SchemaPropertyDef,
): boolean {
  if (prop?.["x-secret"] === true || prop?.format === "password") {
    return true;
  }
  const lower = key.toLowerCase();
  if (
    NON_SECRET_METADATA_KEYS.has(lower) ||
    lower.startsWith("has_") ||
    lower.endsWith("_hint")
  ) {
    return false;
  }
  return SENSITIVE_FIELD_KEYWORDS.some((kw) => lower.includes(kw));
}

function hasStoredSecretFor(
  key: string,
  isEdit: boolean,
  existingConfig?: Record<string, unknown>,
): boolean {
  if (!isEdit || !existingConfig) return false;
  const hint = existingConfig[`${key}_hint`];
  return Boolean(
    existingConfig[`has_${key}`] === true ||
      (typeof hint === "string" && hint.length > 0),
  );
}

export function parseObjectSchema(
  raw: Record<string, unknown> | null | undefined,
): ParsedObjectSchema {
  if (!isPlainRecord(raw)) {
    return { properties: [], required: new Set() };
  }
  const required = new Set<string>(
    Array.isArray(raw.required)
      ? raw.required.filter((x): x is string => typeof x === "string")
      : [],
  );
  const properties: Array<[string, SchemaPropertyDef]> = [];
  if (isPlainRecord(raw.properties)) {
    for (const [key, propVal] of Object.entries(raw.properties)) {
      if (!isPlainRecord(propVal)) continue;
      properties.push([key, propVal as SchemaPropertyDef]);
    }
  }
  return { properties, required };
}

export function initSchemaFieldValues(
  schema: Record<string, unknown> | null | undefined,
  existingConfig?: Record<string, unknown>,
  allowedHosts?: readonly string[],
): SchemaFieldValues {
  const parsed = parseObjectSchema(schema);
  const initial: SchemaFieldValues = {};
  for (const [key, prop] of parsed.properties) {
    const existing = existingConfig?.[key];
    if (prop.type === "boolean") {
      initial[key] =
        typeof existing === "boolean"
          ? existing
          : typeof prop.default === "boolean"
            ? prop.default
            : false;
    } else if (prop.type === "array") {
      const arr = Array.isArray(existing)
        ? existing
        : Array.isArray(prop.default)
          ? prop.default
          : [];
      initial[key] = arr.map((item) => String(item)).join("\n");
    } else if (isSchemaSecretProperty(key, prop)) {
      initial[key] = "";
    } else if (existing !== undefined && existing !== null) {
      initial[key] = String(existing);
    } else if (prop.default !== undefined && prop.default !== null) {
      if (prop.format === "uri") {
        initial[key] = isUriDefaultAllowed(prop.default, allowedHosts)
          ? String(prop.default)
          : "";
      } else {
        initial[key] = String(prop.default);
      }
    } else if (
      parsed.required.has(key) &&
      Array.isArray(prop.enum) &&
      prop.enum.length > 0
    ) {
      initial[key] = String(prop.enum[0]);
    } else {
      initial[key] = "";
    }
  }
  return initial;
}

export function buildValidatedSchemaConfig(
  schema: Record<string, unknown> | null | undefined,
  values: SchemaFieldValues,
  isEdit: boolean,
  existingConfig: Record<string, unknown> | undefined,
  m: Messages,
): { config: Record<string, unknown> | null; error: string | null } {
  const parsed = parseObjectSchema(schema);
  const config: Record<string, unknown> = {};

  for (const [key, prop] of parsed.properties) {
    const label = prop.title || key;
    const isRequired = parsed.required.has(key);
    const isSecret = isSchemaSecretProperty(key, prop);
    const rawVal = values[key];

    if (prop.type === "boolean") {
      config[key] = Boolean(rawVal);
      continue;
    }

    const strVal = typeof rawVal === "string" ? rawVal : "";

    if (prop.type === "array") {
      const items = strVal
        .split(/[\n,]+/)
        .map((s) => s.trim())
        .filter(Boolean);
      if (isRequired && items.length === 0) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldRequired(label),
        };
      }
      if (items.length > 0 || isEdit) {
        config[key] = items;
      }
      continue;
    }

    if (Array.isArray(prop.enum) && prop.enum.length > 0) {
      if (!strVal) {
        if (isRequired) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldRequired(label),
          };
        }
        if (
          isEdit &&
          existingConfig &&
          existingConfig[key] !== undefined &&
          existingConfig[key] !== null
        ) {
          config[key] = null;
        }
        continue;
      }
      const matched = prop.enum.find((opt) => String(opt) === strVal);
      config[key] = matched !== undefined ? matched : strVal;
      continue;
    }

    if (prop.type === "number" || prop.type === "integer") {
      const trimmed = strVal.trim();
      if (!trimmed) {
        if (isRequired) {
          return {
            config: null,
            error: m.plugins.providerModal.errFieldRequired(label),
          };
        }
        if (
          isEdit &&
          existingConfig &&
          existingConfig[key] !== undefined &&
          existingConfig[key] !== null
        ) {
          config[key] = null;
        }
        continue;
      }
      const num = Number(trimmed);
      if (!Number.isFinite(num)) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldInvalidNumber(label),
        };
      }
      if (prop.type === "integer" && !Number.isInteger(num)) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldInvalidInteger(label),
        };
      }
      config[key] = num;
      continue;
    }

    if (isSecret) {
      if (isEdit && !isRequired && rawVal === null) {
        config[key] = null;
      } else if (strVal.length > 0) {
        config[key] = strVal;
      } else if (isRequired && !hasStoredSecretFor(key, isEdit, existingConfig)) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldRequired(label),
        };
      }
      continue;
    }

    const trimmed = strVal.trim();
    if (!trimmed) {
      if (isRequired) {
        return {
          config: null,
          error: m.plugins.providerModal.errFieldRequired(label),
        };
      }
      if (
        isEdit &&
        existingConfig &&
        existingConfig[key] !== undefined &&
        existingConfig[key] !== null
      ) {
        config[key] = null;
      }
      continue;
    }
    if (prop.format === "uri" && !isSafeHttpUrl(trimmed)) {
      return {
        config: null,
        error: m.plugins.providerModal.errFieldInvalidUrl(label),
      };
    }
    config[key] = trimmed;
  }

  return { config, error: null };
}

export function formatSchemaConfigSummary(
  config: Record<string, unknown>,
): Array<{ key: string; value: string; isSecretHint?: boolean }> {
  const entries: Array<{ key: string; value: string; isSecretHint?: boolean }> =
    [];
  for (const [key, val] of Object.entries(config)) {
    if (val === null || val === undefined || val === "") continue;
    if (key.startsWith("has_") && typeof val === "boolean") continue;
    if (key.endsWith("_hint") && typeof val === "string") {
      entries.push({
        key: key.slice(0, -5),
        value: val,
        isSecretHint: true,
      });
      continue;
    }
    if (typeof val === "boolean") {
      entries.push({ key, value: val ? "true" : "false" });
    } else if (typeof val === "number") {
      entries.push({ key, value: String(val) });
    } else if (typeof val === "string") {
      entries.push({ key, value: val });
    } else if (Array.isArray(val) && val.length > 0) {
      entries.push({ key, value: val.map((v) => String(v)).join(", ") });
    }
  }
  return entries;
}

export function SchemaFormFields({
  idPrefix,
  schema,
  values,
  onChange,
  isEdit = false,
  existingConfig,
  allowedHosts,
}: {
  idPrefix: string;
  schema: Record<string, unknown> | null | undefined;
  values: SchemaFieldValues;
  onChange: (next: SchemaFieldValues) => void;
  isEdit?: boolean;
  existingConfig?: Record<string, unknown>;
  allowedHosts?: readonly string[];
}) {
  const { m } = useI18n();
  const parsed = parseObjectSchema(schema);
  const booleanProps = parsed.properties.filter(
    ([, prop]) => prop.type === "boolean",
  );
  const nonBooleanProps = parsed.properties.filter(
    ([, prop]) => prop.type !== "boolean",
  );

  if (parsed.properties.length === 0) {
    return (
      <p className="text-xs text-zinc-500 dark:text-zinc-400">
        {m.plugins.providerModal.genericNoSchemaHint}
      </p>
    );
  }

  return (
    <div className="space-y-3">
      {nonBooleanProps.map(([key, prop]) => {
        const fieldId = `${idPrefix}-${key}`;
        const label = prop.title || key;
        const isRequired = parsed.required.has(key);
        const isSecret = isSchemaSecretProperty(key, prop);
        const storedSecret = hasStoredSecretFor(key, isEdit, existingConfig);
        const isClearedSecret =
          isSecret && isEdit && !isRequired && values[key] === null;
        const hintRaw = existingConfig?.[`${key}_hint`];
        const hint =
          typeof hintRaw === "string" && hintRaw.length > 0 ? hintRaw : null;
        const strVal = typeof values[key] === "string" ? values[key] : "";

        if (Array.isArray(prop.enum) && prop.enum.length > 0) {
          return (
            <div key={key}>
              <Label htmlFor={fieldId}>
                {label}
                {isRequired && <span className="ml-1 text-rose-500">*</span>}
              </Label>
              <Select
                id={fieldId}
                value={strVal}
                onChange={(e) => onChange({ ...values, [key]: e.target.value })}
                className="mt-1"
              >
                {(!isRequired || !strVal) && (
                  <option value="">
                    {m.plugins.providerModal.genericSelectPlaceholder}
                  </option>
                )}
                {prop.enum.map((opt) => (
                  <option key={String(opt)} value={String(opt)}>
                    {String(opt)}
                  </option>
                ))}
              </Select>
              {prop.description && <FieldHint>{prop.description}</FieldHint>}
            </div>
          );
        }

        if (prop.type === "array") {
          return (
            <div key={key}>
              <Label htmlFor={fieldId}>
                {label}
                {isRequired && <span className="ml-1 text-rose-500">*</span>}
              </Label>
              <Textarea
                id={fieldId}
                value={strVal}
                onChange={(e) => onChange({ ...values, [key]: e.target.value })}
                placeholder={prop.placeholder}
                className="mt-1 min-h-[72px] font-mono text-xs"
              />
              <FieldHint>
                {prop.description || m.plugins.providerModal.genericArrayHint}
              </FieldHint>
            </div>
          );
        }

        const inputType = isSecret
          ? "password"
          : prop.type === "number" || prop.type === "integer"
            ? "number"
            : prop.format === "uri"
              ? "url"
              : "text";

        const fallbackUriPlaceholder =
          !isSecret &&
          prop.format === "uri" &&
          !prop.placeholder &&
          typeof prop.default === "string" &&
          prop.default.trim().length > 0 &&
          !isUriDefaultAllowed(prop.default, allowedHosts)
            ? prop.default.trim()
            : undefined;

        const placeholder = isSecret
          ? isClearedSecret
            ? m.plugins.providerModal.secretWillClearPlaceholder
            : storedSecret
              ? hint
                ? m.plugins.providerModal.signatureStoredPlaceholder(hint)
                : m.plugins.providerModal.genericSecretStoredPlaceholder
              : prop.placeholder ||
                m.plugins.providerModal.genericSecretPlaceholder
          : prop.placeholder || fallbackUriPlaceholder;

        return (
          <div key={key}>
            <Label htmlFor={fieldId}>
              {label}
              {isRequired && <span className="ml-1 text-rose-500">*</span>}
            </Label>
            <Input
              id={fieldId}
              type={inputType}
              step={prop.type === "integer" ? 1 : "any"}
              value={strVal}
              disabled={isClearedSecret}
              onChange={(e) => onChange({ ...values, [key]: e.target.value })}
              placeholder={placeholder}
              className="mt-1 font-mono text-xs"
            />
            {isSecret && isEdit && !isRequired && storedSecret && (
              <label className="mt-1.5 inline-flex cursor-pointer items-center gap-1.5 text-[11px] text-zinc-500 dark:text-zinc-400">
                <input
                  type="checkbox"
                  checked={isClearedSecret}
                  onChange={(e) =>
                    onChange({
                      ...values,
                      [key]: e.target.checked ? null : "",
                    })
                  }
                  className="h-3.5 w-3.5 rounded border-zinc-300 accent-zinc-900 dark:border-zinc-700 dark:accent-zinc-100"
                />
                <span>{m.plugins.providerModal.clearStoredSecretLabel}</span>
              </label>
            )}
            {isSecret && storedSecret && !isClearedSecret ? (
              <FieldHint>
                {prop.description
                  ? `${prop.description} ${m.plugins.providerModal.secretPreserveHint}`
                  : m.plugins.providerModal.secretPreserveHint}
              </FieldHint>
            ) : (
              prop.description && <FieldHint>{prop.description}</FieldHint>
            )}
          </div>
        );
      })}

      {booleanProps.length > 0 && (
        <div className="space-y-2.5 rounded-lg border border-zinc-200/80 bg-zinc-50/50 p-3 dark:border-zinc-800 dark:bg-zinc-950/40">
          {booleanProps.map(([key, prop]) => (
            <Switch
              key={key}
              id={`${idPrefix}-bool-${key}`}
              checked={Boolean(values[key])}
              onCheckedChange={(checked) =>
                onChange({ ...values, [key]: checked })
              }
              label={prop.title || key}
              description={prop.description}
            />
          ))}
        </div>
      )}
    </div>
  );
}
