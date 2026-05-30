import { type RefObject, useCallback, useRef, useState } from "react";
import { validateAppConfig } from "../lib/configValidation";
import { type AppConfig, configSet } from "../lib/ipc";

export interface AppConfigController {
  config: AppConfig | null;
  configError: string | null;
  /** Stable ref to the latest config for listeners that must not re-bind. */
  configRef: RefObject<AppConfig | null>;
  /** Install the authoritative config loaded from the backend at launch. */
  initConfig: (config: AppConfig) => void;
  /** Apply a patch optimistically, then persist; roll back if Rust rejects it. */
  updateConfig: (patch: Partial<AppConfig>) => void;
}

/**
 * Owns app config state and the optimistic-update protocol: validate a patch in
 * the frontend (mirroring the Rust bounds), apply it live, persist it, and on
 * backend rejection roll back to the last persisted config and surface the
 * error — so the displayed and on-disk config never silently diverge.
 */
export function useAppConfig(): AppConfigController {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const configRef = useRef<AppConfig | null>(null);
  const persistedConfigRef = useRef<AppConfig | null>(null);
  const configSaveSeqRef = useRef(0);
  configRef.current = config;

  const initConfig = useCallback((cfg: AppConfig) => {
    setConfig(cfg);
    persistedConfigRef.current = cfg;
  }, []);

  const updateConfig = useCallback((patch: Partial<AppConfig>) => {
    setConfig((prev) => {
      if (!prev) return prev;
      const next = { ...prev, ...patch };
      const validationError = validateAppConfig(next);
      if (validationError) {
        setConfigError(validationError);
        return prev;
      }

      setConfigError(null);
      const saveSeq = ++configSaveSeqRef.current;
      void configSet(next)
        .then(() => {
          persistedConfigRef.current = next;
        })
        .catch((err) => {
          console.error("phantom: failed to persist config", err);
          if (saveSeq === configSaveSeqRef.current) {
            setConfig(persistedConfigRef.current);
            setConfigError(String(err));
          }
        });
      return next;
    });
  }, []);

  return { config, configError, configRef, initConfig, updateConfig };
}
