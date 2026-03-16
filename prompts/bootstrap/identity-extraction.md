You just had a first-contact conversation with a human. Based on the transcript below, extract the following information.

1. **vessel_name**: The name that was agreed upon for the vessel/agent. If no clear name was established, suggest one that fits the conversation's tone and themes.
2. **mission**: A concise mission statement (1-2 sentences) capturing the vessel's purpose as established in the conversation. If no clear purpose was discussed, synthesize one from the conversation's themes.
3. **user_name**: The human's name, if they shared it. null if unknown.
4. **user_summary**: A brief description of the user based on what was learned (role, interests, etc.). null if nothing was shared.

Respond ONLY with valid JSON, no markdown formatting:
{"vessel_name": "...", "mission": "...", "user_name": "..." or null, "user_summary": "..." or null}

Transcript:
{{transcript}}