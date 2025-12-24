use crate::Wrapup;

const STYLE: &str = r#"
        :root {
            --bg-dark: #0f1115;
            --bg-card: #181b21;
            --text-primary: #e0e6ed;
            --text-secondary: #949ba4;
            --accent: #7c3aed;
        }
        
        body {
            font-family: 'Inter', -apple-system, BlinkMacSystemFont, sans-serif;
            background-color: var(--bg-dark);
            color: var(--text-primary);
            margin: 0;
            padding: 0;
            line-height: 1.6;
        }

        .container {
            max_width: 1000px;
            margin: 0 auto;
            padding: 40px 20px;
        }

        header {
            text-align: center;
            margin-bottom: 60px;
        }

        h1 {
            font-size: 3.5rem;
            font-weight: 800;
            background: linear-gradient(135deg, #fff 0%, #a78bfa 100%);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            margin: 0;
        }

        .grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
            gap: 24px;
            margin-bottom: 40px;
        }

        .card {
            background: var(--bg-card);
            border: 1px solid #2d333b;
            border-radius: 16px;
            padding: 24px;
        }

        .metric-value {
            font-size: 2.5rem;
            font-weight: 700;
            color: #fff;
        }

        .share-section {
            margin: 60px 0;
            text-align: center;
        }

        /* Bento Box Share Card */
        .share-card {
            width: 800px;
            height: 500px;
            margin: 0 auto 20px auto;
            background: #0f1115;
            border-radius: 32px;
            padding: 30px;
            position: relative;
            color: #fff;
            box-shadow: 0 50px 100px -20px rgba(0,0,0,0.5);
            font-family: 'Inter', sans-serif;
            overflow: hidden;
            display: flex;
            flex-direction: column;
            border: 1px solid #333;
        }
        
        .share-card::before {
             content: '';
             position: absolute;
             top: 0;
             left: 0;
             right: 0;
             height: 4px;
             background: linear-gradient(90deg, #7c3aed, #db2777, #f59e0b);
        }

        .bento-header {
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-bottom: 20px;
        }
        .bento-title {
            font-size: 1.2rem;
            font-weight: 700;
            letter-spacing: -0.5px;
        }
        .bento-subtitle {
            font-size: 0.9rem;
            color: #666;
            text-transform: uppercase;
            letter-spacing: 1px;
        }

        .bento-grid {
            display: grid;
            grid-template-columns: 2fr 1fr 1fr;
            grid-template-rows: 1.5fr 1fr;
            gap: 16px;
            flex: 1;
        }

        .bento-item {
            background: #181b21;
            border-radius: 16px;
            padding: 20px;
            display: flex;
            flex-direction: column;
            justify-content: space-between;
            position: relative;
        }
        
        .bento-item.hero {
            grid-column: 1 / 2;
            grid-row: 1 / 3;
            background: linear-gradient(135deg, #1e1b4b 0%, #0f1115 100%);
            border: 1px solid #2e1065;
        }

        .bento-label {
            font-size: 0.8rem;
            color: #949ba4;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            font-weight: 600;
        }

        .bento-value {
            font-size: 2rem;
            font-weight: 700;
            line-height: 1;
            margin-top: 5px;
        }

        .bento-value.small { font-size: 1.5rem; }
        .bento-value.text { font-size: 1.25rem; word-break: break-word; }

        .sparkline-container {
            flex: 1;
            width: 100%;
            margin-top: 10px;
            position: relative;
        }

        .share-footer {
            margin-top: 20px;
            display: flex;
            align-items: center;
            justify-content: space-between;
            font-size: 0.8rem;
            color: #444;
            font-weight: 600;
        }

        button.download-btn {
            background: var(--accent);
            color: white;
            border: none;
            padding: 12px 24px;
            border-radius: 8px;
            font-size: 1rem;
            cursor: pointer;
            font-weight: 600;
            transition: opacity 0.2s;
        }
        
        button.download-btn:hover { opacity: 0.9; }

        .chart-container { position: relative; height: 300px; width: 100%; }
        .wide-chart { height: 200px; width: 100%; }
"#;

const SCRIPTS_TEMPLATE: &str = r#"
<script>
    const data = JSON_DATA_PLACEHOLDER;

    function downloadImage() {
        const node = document.getElementById('capture-card');
        html2canvas(node, { scale: 2, backgroundColor: '#0f1115' }).then(canvas => {
            const link = document.createElement('a');
            link.download = 'my-ai-year.png';
            link.href = canvas.toDataURL();
            link.click();
        });
    }

    // Helper to get color
    function getGradient(ctx, colorStart, colorEnd) {
        const gradient = ctx.createLinearGradient(0, 0, 0, 400);
        gradient.addColorStop(0, colorStart);
        gradient.addColorStop(1, colorEnd);
        return gradient;
    }

    // Card Sparkline (Coding Clock)
    const ctxCard = document.getElementById('cardSparkline').getContext('2d');
    new Chart(ctxCard, {
        type: 'line',
        data: {
            labels: Array.from({length: 24}, (_, i) => i),
            datasets: [{
                data: data.hourly_activity,
                borderColor: '#a78bfa',
                backgroundColor: 'rgba(124, 58, 237, 0.1)',
                borderWidth: 2,
                tension: 0.4,
                pointRadius: 0,
                fill: true
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false }, tooltip: { enabled: false } },
            scales: {
                y: { display: false },
                x: { display: false }
            },
            layout: { padding: 0 }
        }
    });

    // Tool Chart
    const ctxTool = document.getElementById('toolChart').getContext('2d');
    new Chart(ctxTool, {
        type: 'bar',
        data: {
            labels: data.sessions_by_tool.map(x => x.key),
            datasets: [{
                label: 'Sessions',
                data: data.sessions_by_tool.map(x => x.count),
                backgroundColor: '#7c3aed',
                borderRadius: 4
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false } },
            scales: {
                y: { beginAtZero: true, grid: { color: '#2d333b' } },
                x: { grid: { display: false } }
            }
        }
    });

    // Model Chart
    const ctxModel = document.getElementById('modelChart').getContext('2d');
    new Chart(ctxModel, {
        type: 'doughnut',
        data: {
            labels: data.top_models.map(x => x.key),
            datasets: [{
                data: data.top_models.map(x => x.count),
                backgroundColor: ['#c4b5fd', '#a78bfa', '#8b5cf6', '#7c3aed', '#6d28d9', '#5b21b6'],
                borderWidth: 0
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { 
                legend: { position: 'bottom', labels: { color: '#949ba4', boxWidth: 10 } } 
            }
        }
    });

    // Hourly Chart
    const ctxHourly = document.getElementById('hourlyChart').getContext('2d');
    new Chart(ctxHourly, {
        type: 'line',
        data: {
            labels: Array.from({length: 24}, (_, i) => i + ':00'),
            datasets: [{
                label: 'Activity',
                data: data.hourly_activity,
                borderColor: '#22c55e',
                backgroundColor: 'rgba(34, 197, 94, 0.1)',
                tension: 0.4,
                fill: true
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false } },
            scales: {
                y: { display: false, grid: { display: false } },
                x: { grid: { color: '#2d333b' } }
            }
        }
    });

    // Daily Chart (Intensity)
    const ctxDaily = document.getElementById('dailyChart').getContext('2d');
    new Chart(ctxDaily, {
        type: 'bar',
        data: {
            labels: data.daily_activity.map(x => x[0]),
            datasets: [{
                label: 'Turns',
                data: data.daily_activity.map(x => x[1]),
                backgroundColor: '#a78bfa',
                barPercentage: 1.0,
                categoryPercentage: 1.0
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false }, tooltip: { intersect: false } },
            scales: {
                y: { display: false },
                x: { display: false }
            }
        }
    });
</script>
"#;

pub fn generate_html_report(wrapup: &Wrapup) -> String {
    let json_data = serde_json::to_string(&wrapup).unwrap_or_else(|_| "{}".to_string());
    
    // Determine personality
    let personality = determine_personality(wrapup);
    let badges = determine_badges(wrapup);
    let scripts = SCRIPTS_TEMPLATE.replace("JSON_DATA_PLACEHOLDER", &json_data);

    let top_model = wrapup.top_models.first().map(|x| x.key.as_str()).unwrap_or("None");

    // "Books Written" roughly 500k words per Encyclopedia, 1 token ~ 0.75 words -> 666k tokens = 1 book (approx)
    let books_equivalent = wrapup.tokens.total_tokens as f64 / 750_000.0;

    let marathon_duration = wrapup.longest_session_by_duration.as_ref().map(|s| s.duration_seconds / 60).unwrap_or(0);
    let marathon_hrs = marathon_duration / 60;
    let marathon_mins = marathon_duration % 60;
    let marathon_str = if marathon_hrs > 0 { format!("{}h {}m", marathon_hrs, marathon_mins) } else { format!("{}m", marathon_mins) };

    let top_lang = wrapup.languages.first().map(|x| x.key.as_str()).unwrap_or("None");

    format!(
r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>AI Year in Review {}</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <script src="https://html2canvas.hertzen.com/dist/html2canvas.min.js"></script>
    <style>{}</style>
</head>
<body>

<div class="container">
    <header>
        <div style="color: var(--text-secondary)">CONTRAIL TELEMETRY</div>
        <h1>AI Year in Review {}</h1>
        <div style="color: var(--text-secondary)">{} to {}</div>
    </header>

    <div class="grid">
        <div class="card" style="grid-column: 1 / -1; background: linear-gradient(135deg, #2e1065 0%, #1e1b4b 100%); border-color: #5b21b6; text-align: center;">
            <div style="color: #a78bfa; text-transform: uppercase; letter-spacing: 0.05em; font-size: 0.875rem;">Your Coding Archetype</div>
            <div style="font-size: 3rem; font-weight: 800; color: #fff; margin: 10px 0;">{}</div>
            <p style="color: #c4b5fd; max-width: 600px; margin: 0 auto;">{}</p>
            <div style="margin-top: 20px;">{}</div>
        </div>
    </div>
    
    <!-- Shareable Card Section -->
    <div class="share-section">
        <h2 style="margin-bottom: 20px;">Your 2025 Snapshot</h2>
        <div id="capture-card" class="share-card">
            <div class="bento-header">
                <div class="bento-title">My Year In Code</div>
                <div class="bento-subtitle">2025 Wrapup</div>
            </div>
            
            <div class="bento-grid">
                <!-- Hero: Coding Clock -->
                <div class="bento-item hero">
                    <div class="bento-label">Coding Clock</div>
                    <div class="sparkline-container">
                        <canvas id="cardSparkline"></canvas>
                    </div>
                </div>

                <!-- KPI 1 -->
                <div class="bento-item">
                    <div class="bento-label">Prompts</div>
                    <div class="bento-value">{:.1}K</div>
                </div>

                <!-- KPI 2 -->
                <div class="bento-item">
                    <div class="bento-label">Stats</div>
                    <div class="bento-value">{:.1}B <span style="font-size: 0.5em; opacity: 0.7; vertical-align: middle;">Tokens</span></div>
                    <div style="font-size: 0.8em; color: #7c3aed; margin-top: 5px;">~{:.1} Books Written</div>
                </div>

                <!-- Detail 1: Projects -->
                <div class="bento-item">
                    <div class="bento-label">Projects</div>
                    <div class="bento-value small">{}</div>
                </div>

                <!-- Detail 2: Top Model -->
                <div class="bento-item">
                    <div class="bento-label">Top Model</div>
                    <div class="bento-value text">{}</div>
                </div>
            </div>

            <div class="share-footer">
                <div>Generated by Contrail</div>
                <div>contrail.run</div>
            </div>
        </div>
        <button class="download-btn" onclick="downloadImage()">Download Image</button>
    </div>

    <!-- Stats Row 1 -->
    <div class="grid">
        <div class="card">
            <div style="color: var(--text-secondary); text-transform: uppercase; font-size: 0.875rem;">The Marathon</div>
             <div class="metric-value">{}</div>
             <div style="font-size: 0.9em; color: var(--text-secondary);">Longest continuous session</div>
        </div>
        <div class="card">
            <div style="color: var(--text-secondary); text-transform: uppercase; font-size: 0.875rem;">Tech Stack</div>
            <div class="metric-value">{}</div>
            <div style="font-size: 0.9em; color: var(--text-secondary);">Most edited file type</div>
        </div>
         <div class="card">
            <div style="color: var(--text-secondary); text-transform: uppercase; font-size: 0.875rem;">The Interrupt</div>
            <div class="metric-value">{}</div>
            <div style="font-size: 0.9em; color: var(--text-secondary);">Times you stopped the AI</div>
        </div>
    </div>

    <!-- Charts Row 1 -->
    <div class="grid" style="grid-template-columns: 2fr 1fr;">
        <div class="card">
            <div style="color: var(--text-secondary); margin-bottom: 15px;">Activity by Tool</div>
            <div class="chart-container"><canvas id="toolChart"></canvas></div>
        </div>
        <div class="card">
             <div style="color: var(--text-secondary); margin-bottom: 15px;">Top Models</div>
             <div class="chart-container"><canvas id="modelChart"></canvas></div>
        </div>
    </div>

    <!-- Charts Row 2: Coding Clock -->
    <div class="grid">
        <div class="card">
            <div style="color: var(--text-secondary); margin-bottom: 15px;">The Coding Clock (Hourly Activity)</div>
            <div class="chart-container wide-chart"><canvas id="hourlyChart"></canvas></div>
        </div>
    </div>

    <!-- Charts Row 3: Yearly Intensity -->
    <div class="grid">
        <div class="card">
            <div style="color: var(--text-secondary); margin-bottom: 15px;">Yearly Intensity</div>
            <div class="chart-container wide-chart"><canvas id="dailyChart"></canvas></div>
        </div>
    </div>

    <!-- Productivity Table -->
    <div class="grid">
         <div class="card">
            <div style="color: var(--text-secondary); margin-bottom: 15px;">Productivity Stats</div>
            <div style="display: flex; justify-content: space-between; padding: 10px 0; border-bottom: 1px solid #2d333b;">
                <span>Avg Words/Turn</span>
                <strong>{:.1}</strong>
            </div>
             <div style="display: flex; justify-content: space-between; padding: 10px 0; border-bottom: 1px solid #2d333b;">
                <span>Question Rate</span>
                <strong>{:.1}%</strong>
            </div>
             <div style="display: flex; justify-content: space-between; padding: 10px 0; border-bottom: 1px solid #2d333b;">
                <span>Edits Made</span>
                <strong>{}</strong>
            </div>
             <div style="display: flex; justify-content: space-between; padding: 10px 0;">
                <span>Clipboard Copies</span>
                <strong>{}</strong>
            </div>
        </div>
    </div>
</div>
{}
</body>
</html>
"#,
        // Title
        wrapup.year,
        // Style
        STYLE,
        
        // Header
        wrapup.year,
        wrapup.range_start.map(|d| d.format("%b %d").to_string()).unwrap_or_default(),
        wrapup.range_end.map(|d| d.format("%b %d").to_string()).unwrap_or_default(),
        
        // Personality
        personality.0,
        personality.1,
        badges,

        // BENTO BOX DATA
        wrapup.turns_total as f64 / 1000.0, // Prompts K
        wrapup.tokens.total_tokens as f64 / 1_000_000_000.0, // Tokens B
        books_equivalent,                  // Books
        wrapup.unique_projects,            // Projects
        top_model,                         // Top Model (Text)

        // NEW CARDS
        marathon_str,
        top_lang,
        wrapup.total_interrupts,

        // Productivity (Extra Table)
        wrapup.user_avg_words.unwrap_or(0.0),
        wrapup.user_question_rate.unwrap_or(0.0),
        wrapup.file_effects,                  
        wrapup.clipboard_hits,

        // Scripts
        scripts
    )
}

fn determine_personality(wrapup: &Wrapup) -> (&'static str, &'static str) {
    let q_rate = wrapup.user_question_rate.unwrap_or(0.0);
    let code_rate = wrapup.user_code_hint_rate.unwrap_or(0.0);
    let avg_len = wrapup.user_avg_words.unwrap_or(0.0);
    let total_turns = wrapup.turns_total;

    if total_turns < 50 {
        return ("The Tourist", "You're just passing through, exploring what AI can do.");
    }

    if code_rate > 30.0 {
        return ("The Collaborator", "You treat AI as a true pair programmer, often pasting your own code for review.");
    }

    if q_rate > 40.0 {
        return ("The Interrogator", "You relentlessly question the AI until it yields the truth.");
    }

    if avg_len > 50.0 {
        return ("The Novelist", "Your prompts are detailed, rich stories. You leave nothing to chance.");
    }

    if avg_len < 10.0 {
        return ("The Minimalist", "Short, punchy prompts. You expect the AI to read your mind.");
    }

    if wrapup.antigravity_images > 20 {
        return ("The Voyager", "You use Antigravity to explore new dimensions of code.");
    }

    ("The Architect", "Balanced, focused, and building something great.")
}

fn determine_badges(wrapup: &Wrapup) -> String {
    let mut badges = Vec::new();
    
    if wrapup.longest_streak_days > 7 {
        badges.push(format!("🔥 {}-Day Streak", wrapup.longest_streak_days));
    }
    if wrapup.tokens.total_tokens > 1_000_000 {
        badges.push("💎 Token Millionaire".to_string());
    }
    if wrapup.active_days > 200 {
        badges.push("📅 Daily Driver".to_string());
    }
    if let Some(peak) = wrapup.peak_hour_local {
        if peak < 5 {
            badges.push("🦉 Night Owl".to_string());
        } else if peak < 9 {
            badges.push("☕ Early Bird".to_string());
        }
    }
    if wrapup.file_effects > 1000 {
        badges.push("🚀 Ship It".to_string());
    }

    badges.into_iter().map(|b| format!("<div class=\"badge\">{}</div>", b)).collect::<Vec<_>>().join("\n")
}
