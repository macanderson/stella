"use client";

import * as React from "react";
import { AlertDialog as BaseAlertDialog } from "@base-ui/react/alert-dialog";
import { Button } from "@/components/ui/button";

/**
 * A confirm step for actions that terminate running work. Replaces the old
 * client's window.confirm(), which blocked the whole tab's event loop —
 * including the SSE streams it was asking about.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  confirmLabel: string;
  onConfirm: () => void;
}) {
  return (
    <BaseAlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <BaseAlertDialog.Portal>
        <BaseAlertDialog.Backdrop className="fixed inset-0 z-50 bg-black/55 backdrop-blur-[2px]" />
        <BaseAlertDialog.Popup className="fixed left-1/2 top-1/2 z-50 w-[min(26rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-[12px] border border-line bg-panel p-5 shadow-[0_18px_60px_-18px_rgb(0_0_0/0.7)]">
          <BaseAlertDialog.Title className="text-[15px] font-[650] lowercase">
            {title}
          </BaseAlertDialog.Title>
          <BaseAlertDialog.Description className="mt-2 text-[12.5px] leading-relaxed text-muted">
            {description}
          </BaseAlertDialog.Description>
          <div className="mt-5 flex justify-end gap-2">
            <BaseAlertDialog.Close render={<Button variant="ghost" size="sm" />}>
              keep running
            </BaseAlertDialog.Close>
            <Button
              variant="danger"
              size="sm"
              className="border-bad/50"
              onClick={() => {
                onConfirm();
                onOpenChange(false);
              }}
            >
              {confirmLabel}
            </Button>
          </div>
        </BaseAlertDialog.Popup>
      </BaseAlertDialog.Portal>
    </BaseAlertDialog.Root>
  );
}
