import { useMutation } from "@tanstack/react-query";
import {
  ArrowLeft,
  KeyRound,
  Lock,
  Monitor,
  Moon,
  ShieldCheck,
  Smartphone,
  Sun,
} from "lucide-react";
import { type FormEvent, useState } from "react";
import type { AuthLoginResponse } from "@/api/types";
import { api } from "@/api/client";
import {
  FeedCourierMark,
  useThemeMode,
} from "@/components/layout/AppShell";
import { ComplianceFooter } from "@/components/layout/ComplianceFooter";
import {
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
} from "@/components/ui/primitives";
import { useToast } from "@/components/ui/toast";
import { LocaleSwitchButton, useI18n } from "@/i18n";
import { errorMessage } from "@/lib/utils";

const DEFAULT_ADMIN_USERNAME = "Courier";

export interface AuthGateViewProps {
  setupRequired: boolean;
  version?: string;
  repoUrl?: string;
  license?: string;
  onAuthenticated: () => void;
}

export function AuthGateView({
  setupRequired,
  version,
  repoUrl,
  license,
  onAuthenticated,
}: AuthGateViewProps) {
  const { themeMode, toggleTheme } = useThemeMode();
  const { m } = useI18n();
  const toast = useToast();
  const [username, setUsername] = useState(setupRequired ? DEFAULT_ADMIN_USERNAME : "");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);

  // Second step (TOTP / recovery code) state.
  const [mfaToken, setMfaToken] = useState<string | null>(null);
  const [useRecovery, setUseRecovery] = useState(false);
  const [mfaCode, setMfaCode] = useState("");

  const finishSignIn = () => {
    toast.success(setupRequired ? m.auth.toastInitDone : m.auth.toastSignedIn);
    setPassword("");
    setConfirmPassword("");
    setMfaToken(null);
    setMfaCode("");
    onAuthenticated();
  };

  const authMutation = useMutation({
    mutationFn: async (): Promise<AuthLoginResponse | undefined> => {
      if (setupRequired) {
        await api.auth.setup({
          username: username.trim() || DEFAULT_ADMIN_USERNAME,
          password,
        });
        return undefined;
      }
      return api.auth.login({ username: username.trim(), password });
    },
    onSuccess: (res) => {
      if (res?.mfa_required && res.mfa_token) {
        setMfaToken(res.mfa_token);
        setMfaCode("");
        setUseRecovery(false);
        setPassword("");
        return;
      }
      finishSignIn();
    },
    onError: (err) => {
      setValidationError(errorMessage(err));
    },
  });

  const mfaMutation = useMutation({
    mutationFn: (code: string) => {
      if (!mfaToken) throw new Error(m.auth.mfaExpired);
      return api.auth.login2fa({ mfa_token: mfaToken, code });
    },
    onSuccess: finishSignIn,
    onError: (err) => {
      setValidationError(errorMessage(err));
      setMfaCode("");
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);

    if (setupRequired) {
      if (!username.trim() || username.trim().length > 64) {
        setValidationError(m.auth.errUsername);
        return;
      }
      if (password.length < 8) {
        setValidationError(m.auth.errMinLength);
        return;
      }
      if (password !== confirmPassword) {
        setValidationError(m.auth.errMismatch);
        return;
      }
    } else {
      if (!username.trim()) {
        setValidationError(m.auth.errEmptyUsername);
        return;
      }
      if (!password) {
        setValidationError(m.auth.errEmpty);
        return;
      }
    }

    authMutation.mutate();
  };

  const submitMfa = (code: string) => {
    const cleaned = code.trim();
    if (!cleaned || mfaMutation.isPending) return;
    setValidationError(null);
    mfaMutation.mutate(cleaned);
  };

  const handleMfaSubmit = (e: FormEvent) => {
    e.preventDefault();
    submitMfa(mfaCode);
  };

  const handleTotpChange = (raw: string) => {
    const digits = raw.replace(/\D/g, "").slice(0, 6);
    setMfaCode(digits);
    setValidationError(null);
    if (digits.length === 6) submitMfa(digits);
  };

  const leaveMfaStep = () => {
    setMfaToken(null);
    setMfaCode("");
    setUseRecovery(false);
    setValidationError(null);
  };

  const inMfaStep = mfaToken !== null && !setupRequired;

  return (
    <div className="flex min-h-screen flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <div className="flex items-center justify-end gap-2 px-4 pt-4">
        <LocaleSwitchButton />
        <button
          type="button"
          onClick={toggleTheme}
          title={m.theme.toggleTitle}
          aria-label={m.theme.toggleTitle}
          className="inline-flex cursor-pointer items-center justify-center gap-1.5 rounded-md border border-zinc-200 bg-white px-2.5 py-1.5 text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800"
        >
          {themeMode === "system" ? (
            <>
              <Monitor className="h-3.5 w-3.5 text-emerald-500" />
              {m.theme.system}
            </>
          ) : themeMode === "light" ? (
            <>
              <Sun className="h-3.5 w-3.5 text-amber-500" />
              {m.theme.light}
            </>
          ) : (
            <>
              <Moon className="h-3.5 w-3.5 text-indigo-400" />
              {m.theme.dark}
            </>
          )}
        </button>
      </div>

      <div className="flex flex-1 items-center justify-center px-4 py-10">
        <div className="w-full max-w-md space-y-6">
          <div className="flex flex-col items-center text-center">
            <FeedCourierMark className="h-12 w-12" />
            <h1 className="mt-3 text-xl font-bold tracking-tight">
              {m.auth.heading}
            </h1>
            <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              {m.auth.subheading}
            </p>
          </div>

          <Card>
            <CardHeader>
              <div className="flex items-center gap-2">
                {inMfaStep ? (
                  <Smartphone className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                ) : setupRequired ? (
                  <ShieldCheck className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                ) : (
                  <Lock className="h-4 w-4 text-zinc-600 dark:text-zinc-400" />
                )}
                <CardTitle>
                  {inMfaStep
                    ? m.auth.mfaTitle
                    : setupRequired
                      ? m.auth.initTitle
                      : m.auth.loginTitle}
                </CardTitle>
              </div>
              <CardDescription>
                {inMfaStep
                  ? useRecovery
                    ? m.auth.mfaRecoveryDesc
                    : m.auth.mfaDesc
                  : setupRequired
                    ? m.auth.initDesc
                    : m.auth.loginDesc}
              </CardDescription>
            </CardHeader>

            <CardContent>
              {inMfaStep ? (
                <form onSubmit={handleMfaSubmit} className="space-y-4">
                  <div>
                    <Label htmlFor="fc-mfa-code">
                      {useRecovery
                        ? m.auth.mfaRecoveryLabel
                        : m.auth.mfaCodeLabel}
                    </Label>
                    {useRecovery ? (
                      <Input
                        key="recovery"
                        id="fc-mfa-code"
                        name="recovery-code"
                        autoFocus
                        autoComplete="off"
                        autoCapitalize="characters"
                        spellCheck={false}
                        value={mfaCode}
                        onChange={(e) => {
                          setMfaCode(e.target.value);
                          setValidationError(null);
                        }}
                        placeholder="XXXX-XXXX-XXXX"
                        className="mt-1.5 font-mono tracking-wider"
                      />
                    ) : (
                      <Input
                        key="totp"
                        id="fc-mfa-code"
                        name="otp"
                        autoFocus
                        inputMode="numeric"
                        pattern="[0-9]*"
                        maxLength={6}
                        autoComplete="one-time-code"
                        value={mfaCode}
                        onChange={(e) => handleTotpChange(e.target.value)}
                        placeholder="123456"
                        className="mt-1.5 text-center font-mono text-lg tracking-[0.5em]"
                      />
                    )}
                    <FieldHint>
                      {useRecovery
                        ? m.auth.mfaRecoveryHint
                        : m.auth.mfaCodeHint}
                    </FieldHint>
                  </div>

                  <FieldError message={validationError} />

                  <Button
                    type="submit"
                    className="w-full"
                    loading={mfaMutation.isPending}
                    disabled={!mfaCode.trim()}
                  >
                    <KeyRound className="h-4 w-4" />
                    {m.auth.mfaVerify}
                  </Button>

                  <div className="flex items-center justify-between gap-2 text-xs">
                    <button
                      type="button"
                      onClick={leaveMfaStep}
                      className="inline-flex cursor-pointer items-center gap-1 text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-100"
                    >
                      <ArrowLeft className="h-3 w-3" />
                      {m.auth.mfaBack}
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setUseRecovery((v) => !v);
                        setMfaCode("");
                        setValidationError(null);
                      }}
                      className="cursor-pointer font-medium text-zinc-700 underline underline-offset-2 hover:text-zinc-900 dark:text-zinc-300 dark:hover:text-zinc-100"
                    >
                      {useRecovery ? m.auth.mfaUseTotp : m.auth.mfaUseRecovery}
                    </button>
                  </div>
                </form>
              ) : (
                <form onSubmit={handleSubmit} className="space-y-4">
                  <div>
                    <Label htmlFor="fc-username">{m.auth.usernameLabel}</Label>
                    <Input
                      id="fc-username"
                      name="username"
                      type="text"
                      autoComplete="username"
                      autoCapitalize="none"
                      spellCheck={false}
                      autoFocus={!setupRequired}
                      maxLength={64}
                      value={username}
                      onChange={(e) => {
                        setUsername(e.target.value);
                        setValidationError(null);
                      }}
                      placeholder={DEFAULT_ADMIN_USERNAME}
                      className="mt-1.5"
                    />
                    {setupRequired && (
                      <FieldHint>{m.auth.usernameHint}</FieldHint>
                    )}
                  </div>

                  <div>
                    <Label htmlFor="fc-password">
                      {setupRequired
                        ? m.auth.newPasswordLabel
                        : m.auth.passwordLabel}
                    </Label>
                    <Input
                      id="fc-password"
                      name="password"
                      type="password"
                      autoFocus={setupRequired}
                      autoComplete={
                        setupRequired ? "new-password" : "current-password"
                      }
                      value={password}
                      onChange={(e) => {
                        setPassword(e.target.value);
                        setValidationError(null);
                      }}
                      placeholder={
                        setupRequired
                          ? m.auth.minCharsPlaceholder
                          : "••••••••••••"
                      }
                      className="mt-1.5"
                    />
                    {setupRequired && (
                      <FieldHint>
                        {m.auth.envSeedHintPrefix}{" "}
                        <code className="font-mono">ADMIN_PASSWORD</code>{" "}
                        {m.auth.envSeedHintSuffix}
                      </FieldHint>
                    )}
                  </div>

                  {setupRequired && (
                    <div>
                      <Label htmlFor="fc-confirm-password">
                        {m.auth.confirmPasswordLabel}
                      </Label>
                      <Input
                        id="fc-confirm-password"
                        name="confirm-password"
                        type="password"
                        autoComplete="new-password"
                        value={confirmPassword}
                        onChange={(e) => {
                          setConfirmPassword(e.target.value);
                          setValidationError(null);
                        }}
                        placeholder={m.auth.confirmPasswordPlaceholder}
                        className="mt-1.5"
                      />
                    </div>
                  )}

                  <FieldError message={validationError} />

                  <Button
                    type="submit"
                    className="w-full"
                    loading={authMutation.isPending}
                  >
                    <KeyRound className="h-4 w-4" />
                    {setupRequired ? m.auth.initSubmit : m.auth.loginSubmit}
                  </Button>
                </form>
              )}
            </CardContent>
          </Card>
        </div>
      </div>

      <ComplianceFooter
        version={version}
        repoUrl={repoUrl}
        license={license}
      />
    </div>
  );
}
