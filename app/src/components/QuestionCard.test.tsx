// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QuestionCard } from "./QuestionCard";
import type { PendingQuestion } from "../types";

// Mock the Tauri bridge so the card can be tested without a backend. answerQuestion is the
// call we assert against.
const answerQuestion = vi.fn((..._args: unknown[]) => Promise.resolve());
vi.mock("../api", () => ({
  api: {
    answerQuestion: (...args: unknown[]) => answerQuestion(...args),
  },
}));

const SESSION_ID = 42;

function multiSelectPrompt(): PendingQuestion {
  return {
    session_id: SESSION_ID,
    questions: [
      {
        question: "Which features?",
        header: "Features",
        multiSelect: true,
        options: [
          { label: "Auth", description: "" },
          { label: "Billing", description: "" },
          { label: "Search", description: "" },
        ],
      },
    ],
  };
}

function singleSelectPrompt(): PendingQuestion {
  return {
    session_id: SESSION_ID,
    questions: [
      {
        question: "Pick one",
        header: "",
        multiSelect: false,
        options: [
          { label: "Yes", description: "" },
          { label: "No", description: "" },
        ],
      },
    ],
  };
}

afterEach(() => {
  cleanup();
  answerQuestion.mockClear();
});

describe("QuestionCard multi-select", () => {
  it("toggles picked highlight on click without submitting", () => {
    render(<QuestionCard pq={multiSelectPrompt()} onAnswered={() => {}} />);

    const auth = screen.getByText("Auth").closest("button")!;
    const search = screen.getByText("Search").closest("button")!;

    fireEvent.click(auth);
    fireEvent.click(search);
    expect(auth.className).toContain("picked");
    expect(search.className).toContain("picked");

    // Toggling off removes the highlight.
    fireEvent.click(auth);
    expect(auth.className).not.toContain("picked");

    // Clicking options never calls the backend for multi-select — only Enter does.
    expect(answerQuestion).not.toHaveBeenCalled();
  });

  it("renders no confirm button, only a hint", () => {
    render(<QuestionCard pq={multiSelectPrompt()} onAnswered={() => {}} />);
    expect(screen.queryByText(/Send \d+ selected/)).toBeNull();
    expect(screen.getByText(/Enter to send/)).toBeTruthy();
  });

  it("submits sorted selected indices with multiSelect:true on Enter", () => {
    render(<QuestionCard pq={multiSelectPrompt()} onAnswered={() => {}} />);

    // Click in a non-sorted order; the delivered indices must be sorted ascending.
    fireEvent.click(screen.getByText("Search").closest("button")!); // index 2
    fireEvent.click(screen.getByText("Auth").closest("button")!); // index 0

    fireEvent.keyDown(document.body, { key: "Enter" });

    expect(answerQuestion).toHaveBeenCalledTimes(1);
    expect(answerQuestion).toHaveBeenCalledWith(
      SESSION_ID,
      3, // option count
      [0, 2], // sorted selection
      null, // no custom text
      true, // multiSelect
    );
  });

  it("treats Enter with zero selections as a no-op", () => {
    render(<QuestionCard pq={multiSelectPrompt()} onAnswered={() => {}} />);
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(answerQuestion).not.toHaveBeenCalled();
  });

  it("does not submit picks when Enter is pressed in the custom-answer field", () => {
    render(<QuestionCard pq={multiSelectPrompt()} onAnswered={() => {}} />);
    fireEvent.click(screen.getByText("Auth").closest("button")!);

    const input = screen.getByPlaceholderText(
      "…or type your own answer",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "custom idea" } });
    input.focus();
    fireEvent.keyDown(input, { key: "Enter" });

    // Enter in the field submits the typed text, not the picked options.
    expect(answerQuestion).toHaveBeenCalledTimes(1);
    expect(answerQuestion).toHaveBeenCalledWith(
      SESSION_ID,
      3,
      [],
      "custom idea",
      true,
    );
  });
});

describe("QuestionCard single-select (unchanged behavior)", () => {
  it("submits immediately on option click", () => {
    render(<QuestionCard pq={singleSelectPrompt()} onAnswered={() => {}} />);
    fireEvent.click(screen.getByText("No").closest("button")!); // index 1

    expect(answerQuestion).toHaveBeenCalledTimes(1);
    expect(answerQuestion).toHaveBeenCalledWith(
      SESSION_ID,
      2,
      [1],
      null,
      false,
    );
  });

  it("does not submit on Enter (no multi-select handler)", () => {
    render(<QuestionCard pq={singleSelectPrompt()} onAnswered={() => {}} />);
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(answerQuestion).not.toHaveBeenCalled();
  });
});
