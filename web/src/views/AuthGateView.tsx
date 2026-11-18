import { useMutation } from "@tanstack/react-query";
import { KeyRound, Lock, Monitor, Moon, ShieldCheck, Sun } from "lucide-react";
import { type FormEvent, useState } from "react";
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
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);

  const authMutation = useMutation({
    mutationFn: async (pwd: string) => {
      if (setupRequired) {
        return api.auth.setup({ password: pwd });
      }
      return api.auth.login({ password: pwd });
    },
    onSuccess: () => {
      toast.success(
        setupRequired ? m.auth.toastInitDone : m.auth.toastSignedIn,
      );
      setPassword("");
      setConfirmPassword("");
      onAuthenticated();
    },
    onError: (err) => {
      setValidationError(errorMessage(err));
    },
  });

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    setValidationError(null);

    if (setupRequired) {
      if (password.length < 8) {
        setValidationError(m.auth.errMinLength);
        return;
      }
      if (password !== confirmPassword) {
        setValidationError(m.auth.errMismatch);
        return;
      }
    } else if (!password) {
      setValidationError(m.auth.errEmpty);
      return;
    }

    authMutation.mutate(password);
  };

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
                {setupRequired ? (
                  <ShieldCheck className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                ) : (
                  <Lock className="h-4 w-4 text-zinc-600 dark:text-zinc-400" />
                )}
                <CardTitle>
                  {setupRequired ? m.auth.initTitle : m.auth.loginTitle}
                </CardTitle>
              </div>
              <CardDescription>
                {setupRequired ? m.auth.initDesc : m.auth.loginDesc}
              </CardDescription>
            </CardHeader>

            <CardContent>
              <form onSubmit={handleSubmit} className="space-y-4">
                <div>
                  <Label htmlFor="fc-password">
                    {setupRequired
                      ? m.auth.newPasswordLabel
                      : m.auth.passwordLabel}
                  </Label>
                  <Input
                    id="fc-password"
                    type="password"
                    autoFocus
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
