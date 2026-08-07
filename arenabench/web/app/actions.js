"use server";

import { revalidatePath } from "next/cache";
import { startSutBuild, submitSmoke } from "../lib/aws";

export async function buildSutAction(formData) {
  const ref = String(formData.get("ref") || "main").trim() || "main";
  await startSutBuild(ref);
  revalidatePath("/");
}

export async function smokeAction() {
  await submitSmoke();
  revalidatePath("/");
}
