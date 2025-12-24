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

        /* Vibrant Bento V2 Share Card */
        .share-card {
            width: 900px;
            height: 600px;
            margin: 0 auto 20px auto;
            background: #0f1115;
            border-radius: 40px;
            padding: 40px;
            position: relative;
            color: #fff;
            box-shadow: 0 50px 100px -20px rgba(0,0,0,0.5);
            font-family: 'Inter', sans-serif;
            overflow: hidden;
            display: flex;
            flex-direction: column;
            border: 1px solid #333;
        }
        
        .share-header {
            display: flex;
            justify-content: space-between;
            align-items: flex-end;
            margin-bottom: 30px;
            border-bottom: 1px solid #333;
            padding-bottom: 20px;
        }

        .share-title h2 { margin: 0; font-size: 2rem; font-weight: 800; line-height: 1; }
        .share-title span { color: #a78bfa; font-size: 1.2rem; font-weight: 600; text-transform: uppercase; letter-spacing: 1px; }

        .share-body {
            display: grid;
            grid-template-columns: 1fr 1.5fr;
            gap: 30px;
            flex: 1;
        }

        .left-col {
            display: flex;
            flex-direction: column;
            gap: 20px;
        }

        .chart-tile {
            background: #181b21;
            border-radius: 24px;
            padding: 20px;
            flex: 1;
            display: flex;
            flex-direction: column;
            position: relative;
            border: 1px solid #2d333b;
        }
        
        .chart-label {
            font-size: 0.8rem;
            text-transform: uppercase;
            letter-spacing: 1px;
            color: #949ba4;
            margin-bottom: 10px;
            font-weight: 600;
        }

        .chart-canvas-wrapper {
            flex: 1;
            position: relative;
            width: 100%;
        }

        .metric-grid {
            display: grid;
            grid-template-columns: 1fr 1fr;
            grid-template-rows: 1fr 1fr 1fr;
            gap: 15px;
        }

        .bubble-tile {
            border-radius: 24px;
            padding: 20px;
            display: flex;
            flex-direction: column;
            justify-content: center;
            color: #000;
            transition: transform 0.2s;
        }

        .bubble-tile h3 { margin: 0; font-size: 2.2rem; font-weight: 800; line-height: 1; }
        .bubble-tile span { font-size: 0.8rem; font-weight: 700; text-transform: uppercase; opacity: 0.7; margin-top: 5px; }

        /* Gradients */
        .g1 { background: linear-gradient(135deg, #a78bfa 0%, #ddd6fe 100%); } /* Prompts */
        .g2 { background: linear-gradient(135deg, #f472b6 0%, #fbcfe8 100%); } /* Tokens */
        .g3 { background: linear-gradient(135deg, #34d399 0%, #a7f3d0 100%); } /* Streak */
        .g4 { background: linear-gradient(135deg, #60a5fa 0%, #bfdbfe 100%); } /* Questions */
        .g5 { background: linear-gradient(135deg, #fbbf24 0%, #fde68a 100%); } /* Interrupts */
        .g6 { background: linear-gradient(135deg, #f87171 0%, #fecaca 100%); } /* Words */

        .share-footer {
            margin-top: 20px;
            text-align: center;
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

    // Card Chart 1: Coding Clock (Line)
    const ctxCardSpark = document.getElementById('cardSparkline').getContext('2d');
    new Chart(ctxCardSpark, {
        type: 'line',
        data: {
            labels: Array.from({length: 24}, (_, i) => i),
            datasets: [{
                data: data.hourly_activity,
                borderColor: '#a78bfa',
                backgroundColor: 'rgba(167, 139, 250, 0.2)',
                borderWidth: 2,
                tension: 0.4,
                pointRadius: 0,
                fill: true
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false } },
            scales: { y: { display: false }, x: { display: false } },
            layout: { padding: 5 }
        }
    });

    // Card Chart 2: Intensity (Bar)
    const ctxCardIntensity = document.getElementById('cardIntensity').getContext('2d');
    new Chart(ctxCardIntensity, {
        type: 'bar',
        data: {
            labels: data.daily_activity.map(x => x[0]),
            datasets: [{
                data: data.daily_activity.map(x => x[1]),
                backgroundColor: '#34d399',
                barPercentage: 1.0, 
                categoryPercentage: 1.0
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            plugins: { legend: { display: false } },
            scales: { y: { display: false }, x: { display: false } },
            layout: { padding: 5 }
        }
    });

    // Tool Chart (Main Page)
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

    // Model Chart (Main Page)
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

    // Hourly (Main Page)
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

    // Daily (Main Page)
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
    
    <!-- Shareable Card Section -->
    <div class="share-section">
        <h2 style="margin-bottom: 20px;">Your 2025 Snapshot</h2>
        <div id="capture-card" class="share-card">
            <div class="share-header">
                <div class="share-title">
                    <span>My Year In Code</span>
                    <h2>{}</h2> 
                </div>
                <div style="font-size: 1.5rem; font-weight: 700;">2025</div>
            </div>
            
            <div class="share-body">
                <div class="left-col">
                    <div class="chart-tile">
                        <div class="chart-label">The Coding Clock</div>
                        <div class="chart-canvas-wrapper"><canvas id="cardSparkline"></canvas></div>
                    </div>
                    <div class="chart-tile">
                        <div class="chart-label">Yearly Intensity</div>
                        <div class="chart-canvas-wrapper"><canvas id="cardIntensity"></canvas></div>
                    </div>
                </div>

                <div class="metric-grid">
                     <div class="bubble-tile g1">
                        <h3>{:.1}K</h3>
                        <span>Prompts</span>
                    </div>
                    <div class="bubble-tile g2">
                        <h3>{:.1}B</h3>
                        <span>Tokens</span>
                    </div>
                    <div class="bubble-tile g3">
                        <h3>{}</h3>
                        <span>Streak (Days)</span>
                    </div>
                     <div class="bubble-tile g4">
                        <h3>{:.0}%</h3>
                        <span>Questions</span>
                    </div>
                    <div class="bubble-tile g5">
                        <h3>{}</h3>
                        <span>Interrupts</span>
                    </div>
                    <div class="bubble-tile g6">
                        <h3>{:.0}</h3>
                        <span>Avg Words</span>
                    </div>
                </div>
            </div>

            <div class="share-footer">
                contrail.run • Generated on {}
            </div>
        </div>
        <button class="download-btn" onclick="downloadImage()">Download Image</button>
    </div>

    <!-- Additional Stats Cards -->
     <div class="grid">
        <div class="card" style="background: linear-gradient(135deg, #2e1065 0%, #1e1b4b 100%); border-color: #5b21b6; text-align: center;">
             <div style="color: #a78bfa; text-transform: uppercase;">Your Archetype</div>
             <div style="font-size: 2rem; font-weight: 800; color: #fff;">{}</div>
             <p style="color: #c4b5fd;">{}</p>
        </div>
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
        
        // SHARE HEADER: Personality Name
        personality.0, 

        // METRICS GRID (6 TILES)
        wrapup.turns_total as f64 / 1000.0, // Prompts K
        wrapup.tokens.total_tokens as f64 / 1_000_000_000.0, // Tokens B
        wrapup.longest_streak_days,        // Streak
        wrapup.user_question_rate.unwrap_or(0.0), // Question Rate
        wrapup.total_interrupts, // Interrupts
        wrapup.user_avg_words.unwrap_or(0.0), // Avg Words

        // Footer Date
        chrono::Local::now().format("%b %d, %Y"),

        // Bottom Cards
        personality.0,
        personality.1,
        marathon_str,
        top_lang,

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
