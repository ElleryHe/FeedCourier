import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
  Copy,
  Download,
  KeyRound,
  RefreshCw,
  Save,
  ShieldCheck,
  ShieldOff,
  Smartphone,
  UserCog,
} from "lucide-react";
import { type FormEvent, useState } from "react";
import { api } from "@/api/client";
import type { AccountDto, TotpSetupResponse } from "@/api/types";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  FieldError,
  FieldHint,
  Input,
  Label,
  Modal,
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { useI18n } from "@/i18n";
import { copyToClipboard, errorMessage } from "@/lib/utils";

function downloadTextFile(filename: string, text: string): void {
  const blob = new Blob([text], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 5000);
}

/** Renders backend-provided inline SVG through an isolated `<img>` (scripts never execute). */
function QrImage({ svg, alt }: { svg: string; alt: string }) {
  const src = `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
  return (
    <img
      src={src}
      alt={alt}
      className="h-48 w-48 rounded-lg border border-zinc-200 bg-white p-2 dark:border-zinc-700"
    />
  );
}

// ---------------------------------------------------------------------------
// Recovery codes (shown exactly once)
// ---------------------------------------------------------------------------

function RecoveryCodesPanel({ codes }: { codes: string[] }) {
  const { m } = useI18n();
  const toast = useToast();
  const [copied, setCopied] = useState(false);
  const text = codes.join("\n");

  const handleCopy = async () => {
    if (await copyToClipboard(text)) {
      setCopied(true);
      toast.success(m.settings.account.codesCopied);
      window.setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="space-y-3">
      <div className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-200">
        {m.settings.account.codesWarning}
      </div>
      <div className="grid grid-cols-2 gap-2 rounded-lg border border-zinc-200 bg-zinc-50 p-3 font-mono text-sm dark:border-zinc-800 dark:bg-zinc-950">
        {codes.map((code) => (
          <code key={code} className="select-all tracking-wide">
            {code}
          </code>
        ))}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" onClick={() => void handleCopy()}>
          {copied ? (
            <Check className="h-3.5 w-3.5 text-emerald-600" />
          ) : (
            <Copy className="h-3.5 w-3.5" />
          )}
          {m.settings.account.codesCopy}
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() =>
            downloadTextFile(
              "feedcourier-recovery-codes.txt",
              `FeedCourier recovery codes\n\n${text}\n`,
            )
          }
        >
          <Download className="h-3.5 w-3.5" />
          {m.settings.account.codesDownload}
        </Button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Enable 2FA wizard: password -> scan + confirm -> recovery codes
// ---------------------------------------------------------------------------

function EnableTotpModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [step, setStep] = useState<"password" | "scan" | "codes">("password");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [setup, setSetup] = useState<TotpSetupResponse | null>(null);
  const [codes, setCodes] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [secretCopied, setSecretCopied] = useState(false);

  const setupMutation = useMutation({
    mutationFn: () => api.account.totpSetup({ current_password: password }),
    onSuccess: (res) => {
      setSetup(res);
      setPassword("");
      setStep("scan");
      setError(null);
    },
    onError: (err) => setError(errorMessage(err)),
  });

  const enableMutation = useMutation({
    mutationFn: (value: string) => api.account.totpEnable({ code: value }),
    onSuccess: (res) => {
      setCodes(res.recovery_codes);
      setSetup(null);
      setStep("codes");
      setError(null);
      toast.success(m.settings.account.toastEnabled);
      void queryClient.invalidateQueries({ queryKey: ["account"] });
    },
    onError: (err) => {
      setError(errorMessage(err));
      setCode("");
    },
  });

  const submitPassword = (e?: FormEvent) => {
    e?.preventDefault();
    setError(null);
    if (!password) {
      setError(m.settings.account.errCurrentPassword);
      return;
    }
    setupMutation.mutate();
  };

  const submitCode = (value: string) => {
    if (enableMutation.isPending) return;
    setError(null);
    enableMutation.mutate(value);
  };

  const handleCodeChange = (raw: string) => {
    const digits = raw.replace(/\D/g, "").slice(0, 6);
    setCode(digits);
    setError(null);
    if (digits.length === 6) submitCode(digits);
  };

  const footer =
    step === "password" ? (
      <>
        <Button variant="outline" onClick={onClose}>
          {m.common.cancel}
        </Button>
        <Button onClick={() => submitPassword()} loading={setupMutation.isPending}>
          {m.settings.account.continueBtn}
        </Button>
      </>
    ) : step === "scan" ? (
      <>
        <Button variant="outline" onClick={onClose}>
          {m.common.cancel}
        </Button>
        <Button
          onClick={() => submitCode(code)}
          loading={enableMutation.isPending}
          disabled={code.length !== 6}
        >
          {m.settings.account.verifyEnable}
        </Button>
      </>
    ) : (
      <Button onClick={onClose}>{m.settings.account.codesSaved}</Button>
    );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        step === "codes"
          ? m.settings.account.codesTitle
          : m.settings.account.enableTitle
      }
      description={
        step === "password"
          ? m.settings.account.enablePasswordDesc
          : step === "scan"
            ? m.settings.account.enableScanDesc
            : m.settings.account.codesDesc
      }
      size="md"
      footer={footer}
    >
      {step === "password" && (
        <form onSubmit={submitPassword} className="space-y-3">
          <div>
            <Label htmlFor="totp-current-password">
              {m.settings.account.currentPasswordLabel}
            </Label>
            <Input
              id="totp-current-password"
              type="password"
              autoComplete="current-password"
              autoFocus
              value={password}
              onChange={(e) => {
                setPassword(e.target.value);
                setError(null);
              }}
              className="mt-1.5"
            />
          </div>
          <FieldError message={error} />
        </form>
      )}

      {step === "scan" && setup && (
        <div className="space-y-4">
          <div className="flex flex-col items-center gap-3 sm:flex-row sm:items-start">
            <QrImage svg={setup.qr_svg} alt={m.settings.account.qrAlt} />
            <div className="min-w-0 flex-1 space-y-2">
              <Label>{m.settings.account.manualSecret}</Label>
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded-md bg-zinc-100 px-2.5 py-1.5 font-mono text-xs break-all select-all dark:bg-zinc-800">
                  {setup.secret}
                </code>
                <Button
                  variant="outline"
                  size="icon"
                  title={m.settings.account.copySecret}
                  aria-label={m.settings.account.copySecret}
                  onClick={() => {
                    void copyToClipboard(setup.secret).then((ok) => {
                      if (ok) {
                        setSecretCopied(true);
                        window.setTimeout(() => setSecretCopied(false), 2000);
                      }
                    });
                  }}
                >
                  {secretCopied ? (
                    <Check className="h-3.5 w-3.5 text-emerald-600" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </Button>
              </div>
              <FieldHint>{m.settings.account.manualSecretHint}</FieldHint>
            </div>
          </div>

          <div>
            <Label htmlFor="totp-confirm-code">
              {m.settings.account.confirmCodeLabel}
            </Label>
            <Input
              id="totp-confirm-code"
              name="otp"
              inputMode="numeric"
              pattern="[0-9]*"
              maxLength={6}
              autoComplete="one-time-code"
              autoFocus
              value={code}
              onChange={(e) => handleCodeChange(e.target.value)}
              placeholder="123456"
              className="mt-1.5 text-center font-mono text-lg tracking-[0.5em]"
            />
            <FieldError message={error} />
          </div>
        </div>
      )}

      {step === "codes" && <RecoveryCodesPanel codes={codes} />}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Regenerate recovery codes: password -> codes
// ---------------------------------------------------------------------------

function RegenerateCodesModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [password, setPassword] = useState("");
  const [codes, setCodes] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () =>
      api.account.regenerateRecoveryCodes({ current_password: password }),
    onSuccess: (res) => {
      setCodes(res.recovery_codes);
      setPassword("");
      toast.success(m.settings.account.toastRegenerated);
      void queryClient.invalidateQueries({ queryKey: ["account"] });
    },
    onError: (err) => setError(errorMessage(err)),
  });

  const submit = (e?: FormEvent) => {
    e?.preventDefault();
    setError(null);
    if (!password) {
      setError(m.settings.account.errCurrentPassword);
      return;
    }
    mutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={
        codes ? m.settings.account.codesTitle : m.settings.account.regenTitle
      }
      description={
        codes ? m.settings.account.codesDesc : m.settings.account.regenDesc
      }
      size="md"
      footer={
        codes ? (
          <Button onClick={onClose}>{m.settings.account.codesSaved}</Button>
        ) : (
          <>
            <Button variant="outline" onClick={onClose}>
              {m.common.cancel}
            </Button>
            <Button onClick={() => submit()} loading={mutation.isPending}>
              {m.settings.account.regenConfirm}
            </Button>
          </>
        )
      }
    >
      {codes ? (
        <RecoveryCodesPanel codes={codes} />
      ) : (
        <form onSubmit={submit} className="space-y-3">
          <div>
            <Label htmlFor="regen-current-password">
              {m.settings.account.currentPasswordLabel}
            </Label>
            <Input
              id="regen-current-password"
              type="password"
              autoComplete="current-password"
              autoFocus
              value={password}
              onChange={(e) => {
                setPassword(e.target.value);
                setError(null);
              }}
              className="mt-1.5"
            />
          </div>
          <FieldError message={error} />
        </form>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Disable 2FA: password + TOTP/recovery code
// ---------------------------------------------------------------------------

function DisableTotpModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () =>
      api.account.totpDisable({
        current_password: password,
        code: code.trim(),
      }),
    onSuccess: (res) => {
      queryClient.setQueryData(["account"], res);
      toast.success(m.settings.account.toastDisabled);
      onClose();
    },
    onError: (err) => setError(errorMessage(err)),
  });

  const submit = (e?: FormEvent) => {
    e?.preventDefault();
    setError(null);
    if (!password) {
      setError(m.settings.account.errCurrentPassword);
      return;
    }
    if (!code.trim()) {
      setError(m.settings.account.errCodeRequired);
      return;
    }
    mutation.mutate();
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={m.settings.account.disableTitle}
      description={m.settings.account.disableDesc}
      size="md"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {m.common.cancel}
          </Button>
          <Button
            variant="destructive"
            onClick={() => submit()}
            loading={mutation.isPending}
          >
            {m.settings.account.disableConfirm}
          </Button>
        </>
      }
    >
      <form onSubmit={submit} className="space-y-3">
        <div>
          <Label htmlFor="disable-current-password">
            {m.settings.account.currentPasswordLabel}
          </Label>
          <Input
            id="disable-current-password"
            type="password"
            autoComplete="current-password"
            autoFocus
            value={password}
            onChange={(e) => {
              setPassword(e.target.value);
              setError(null);
            }}
            className="mt-1.5"
          />
        </div>
        <div>
          <Label htmlFor="disable-code">{m.settings.account.codeLabel}</Label>
          <Input
            id="disable-code"
            name="otp"
            autoComplete="one-time-code"
            spellCheck={false}
            value={code}
            onChange={(e) => {
              setCode(e.target.value);
              setError(null);
            }}
            placeholder="123456 / XXXX-XXXX-XXXX"
            className="mt-1.5 font-mono"
          />
          <FieldHint>{m.settings.account.codeHint}</FieldHint>
        </div>
        <FieldError message={error} />
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Account & Security card
// ---------------------------------------------------------------------------

export function AccountSecurityCard() {
  const { m } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();

  const accountQuery = useQuery({
    queryKey: ["account"],
    queryFn: () => api.account.get(),
  });
  const account: AccountDto | undefined = accountQuery.data;

  const [newUsername, setNewUsername] = useState<string | null>(null);
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [currentPassword, setCurrentPassword] = useState("");
  const [code, setCode] = useState("");
  const [formError, setFormError] = useState<string | null>(null);

  const [enableOpen, setEnableOpen] = useState(false);
  const [regenOpen, setRegenOpen] = useState(false);
  const [disableOpen, setDisableOpen] = useState(false);

  const usernameValue = newUsername ?? account?.username ?? "";
  const usernameChanged =
    !!account &&
    !account.username_env_managed &&
    usernameValue.trim() !== "" &&
    usernameValue.trim() !== account.username;
  const passwordChanging = newPassword.length > 0;

  const updateMutation = useMutation({
    mutationFn: () =>
      api.account.update({
        current_password: currentPassword,
        new_username: usernameChanged ? usernameValue.trim() : undefined,
        new_password: passwordChanging ? newPassword : undefined,
        code:
          passwordChanging && account?.totp_enabled ? code.trim() : undefined,
      }),
    onSuccess: (res) => {
      queryClient.setQueryData(["account"], res);
      toast.success(
        passwordChanging
          ? m.settings.account.toastPasswordChanged
          : m.settings.account.toastUpdated,
      );
      setNewUsername(null);
      setNewPassword("");
      setConfirmPassword("");
      setCurrentPassword("");
      setCode("");
    },
    onError: (err) => setFormError(errorMessage(err)),
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setFormError(null);
    if (!account) return;
    if (!usernameChanged && !passwordChanging) {
      setFormError(m.settings.account.errNothingChanged);
      return;
    }
    if (usernameChanged && usernameValue.trim().length > 64) {
      setFormError(m.settings.account.errUsername);
      return;
    }
    if (passwordChanging) {
      if (newPassword.length < 8) {
        setFormError(m.settings.account.errMinLength);
        return;
      }
      if (newPassword !== confirmPassword) {
        setFormError(m.settings.account.errMismatch);
        return;
      }
      if (account.totp_enabled && !code.trim()) {
        setFormError(m.settings.account.errCodeRequired);
        return;
      }
    }
    if (!currentPassword) {
      setFormError(m.settings.account.errCurrentPassword);
      return;
    }
    updateMutation.mutate();
  };

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <UserCog className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
          <CardTitle>{m.settings.account.title}</CardTitle>
        </div>
        <CardDescription>{m.settings.account.desc}</CardDescription>
      </CardHeader>

      <CardContent className="space-y-5">
        {accountQuery.isLoading || !account ? (
          <div className="py-6 text-center text-xs text-zinc-500">
            {accountQuery.isError
              ? errorMessage(accountQuery.error)
              : m.settings.account.loading}
          </div>
        ) : (
          <>
            {/* Credentials */}
            <form
              onSubmit={handleSubmit}
              className="space-y-3.5 rounded-xl border border-zinc-200 bg-zinc-50/50 p-4 dark:border-zinc-800 dark:bg-zinc-950/50"
            >
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="sm:col-span-2">
                  <Label htmlFor="account-username">
                    {m.settings.account.usernameLabel}
                  </Label>
                  <Input
                    id="account-username"
                    name="username"
                    autoComplete="username"
                    autoCapitalize="none"
                    spellCheck={false}
                    maxLength={64}
                    disabled={account.username_env_managed}
                    value={usernameValue}
                    onChange={(e) => {
                      setNewUsername(e.target.value);
                      setFormError(null);
                    }}
                    className="mt-1.5 sm:max-w-sm"
                  />
                  {account.username_env_managed && (
                    <FieldHint>
                      {m.settings.account.usernameEnvPrefix}{" "}
                      <code className="font-mono">ADMIN_USERNAME</code>{" "}
                      {m.settings.account.usernameEnvSuffix}
                    </FieldHint>
                  )}
                </div>

                <div>
                  <Label htmlFor="account-new-password">
                    {m.settings.account.newPasswordLabel}
                  </Label>
                  <Input
                    id="account-new-password"
                    name="new-password"
                    type="password"
                    autoComplete="new-password"
                    value={newPassword}
                    onChange={(e) => {
                      setNewPassword(e.target.value);
                      setFormError(null);
                    }}
                    placeholder={m.settings.account.newPasswordPlaceholder}
                    className="mt-1.5"
                  />
                </div>
                <div>
                  <Label htmlFor="account-confirm-password">
                    {m.settings.account.confirmPasswordLabel}
                  </Label>
                  <Input
                    id="account-confirm-password"
                    name="confirm-password"
                    type="password"
                    autoComplete="new-password"
                    value={confirmPassword}
                    onChange={(e) => {
                      setConfirmPassword(e.target.value);
                      setFormError(null);
                    }}
                    className="mt-1.5"
                  />
                </div>

                {passwordChanging && account.totp_enabled && (
                  <div className="sm:col-span-2">
                    <Label htmlFor="account-code">
                      {m.settings.account.codeLabel}
                    </Label>
                    <Input
                      id="account-code"
                      name="otp"
                      autoComplete="one-time-code"
                      spellCheck={false}
                      value={code}
                      onChange={(e) => {
                        setCode(e.target.value);
                        setFormError(null);
                      }}
                      placeholder="123456 / XXXX-XXXX-XXXX"
                      className="mt-1.5 font-mono sm:max-w-sm"
                    />
                    <FieldHint>{m.settings.account.codeHint}</FieldHint>
                  </div>
                )}

                <div className="sm:col-span-2">
                  <Label htmlFor="account-current-password">
                    {m.settings.account.currentPasswordLabel}
                  </Label>
                  <Input
                    id="account-current-password"
                    name="current-password"
                    type="password"
                    autoComplete="current-password"
                    value={currentPassword}
                    onChange={(e) => {
                      setCurrentPassword(e.target.value);
                      setFormError(null);
                    }}
                    className="mt-1.5 sm:max-w-sm"
                  />
                  <FieldHint>{m.settings.account.sessionsHint}</FieldHint>
                </div>
              </div>

              <FieldError message={formError} />

              <div className="flex justify-end">
                <Button
                  type="submit"
                  size="sm"
                  loading={updateMutation.isPending}
                >
                  <Save className="h-3.5 w-3.5" />
                  {m.settings.account.saveBtn}
                </Button>
              </div>
            </form>

            {/* Two-factor authentication */}
            <div className="space-y-3 rounded-xl border border-zinc-200 p-4 dark:border-zinc-800">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <Smartphone className="h-4 w-4 text-zinc-600 dark:text-zinc-300" />
                  <h4 className="text-sm font-semibold">
                    {m.settings.account.twoFaTitle}
                  </h4>
                  {account.totp_enabled ? (
                    <Badge variant="success">
                      <ShieldCheck className="h-3 w-3" />
                      {m.settings.account.twoFaEnabled}
                    </Badge>
                  ) : (
                    <Badge variant="default">
                      <ShieldOff className="h-3 w-3" />
                      {m.settings.account.twoFaDisabled}
                    </Badge>
                  )}
                </div>
                {account.totp_enabled && (
                  <Badge variant="mono">
                    {m.settings.account.recoveryRemaining(
                      account.recovery_codes_remaining,
                    )}
                  </Badge>
                )}
              </div>
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                {m.settings.account.twoFaDesc}
              </p>

              <div className="flex flex-wrap gap-2">
                {account.totp_enabled ? (
                  <>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setRegenOpen(true)}
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                      {m.settings.account.regenerate}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setDisableOpen(true)}
                      className="text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-950/40"
                    >
                      <ShieldOff className="h-3.5 w-3.5" />
                      {m.settings.account.disable}
                    </Button>
                  </>
                ) : (
                  <Button size="sm" onClick={() => setEnableOpen(true)}>
                    <KeyRound className="h-3.5 w-3.5" />
                    {m.settings.account.enable}
                  </Button>
                )}
              </div>
            </div>
          </>
        )}
      </CardContent>

      {enableOpen && (
        <EnableTotpModal
          open={enableOpen}
          onClose={() => setEnableOpen(false)}
        />
      )}
      {regenOpen && (
        <RegenerateCodesModal
          open={regenOpen}
          onClose={() => setRegenOpen(false)}
        />
      )}
      {disableOpen && (
        <DisableTotpModal
          open={disableOpen}
          onClose={() => setDisableOpen(false)}
        />
      )}
    </Card>
  );
}
